//! 2D HL animator demo: AnimationSet + State Machine + velocity/facing clips.
//!
//! Отдельный пример (не переписывает `two_d_tile_playground`). WASD / стрелки —
//! idle↔walk и горизонтальный flip; Space — once `attack` → idle.
//!
//! ```text
//! cargo run -p yuyib --example two_d_animator_playable --features "app,two-d"
//! ```

use std::{cell::RefCell, rc::Rc, time::Duration};

use yuyib::{
    animation::{AnimationSet, AnimationStateDef, AnimationStateMachine},
    app::{Application, RenderLoop},
    assets::Assets,
    game_2d::{
        AnimatedSprite2d, Game2dSceneConfig, KinematicSpriteController2d, Sprite2d,
        SpriteAnimator2d, TileCollision2d, TileMap2d, VelocityFacingPolicy2d,
        apply_velocity_facing_2d,
    },
    image::{DecodePolicy, DecodedImage, decode_bytes},
    platform::{WindowConfig, winit},
    profile_2d::{Game2dProfile, PlayableLoop2d, PlayableLoopDesc2d},
    render::ClearColor,
    two_d::{PlaybackMode, SpriteAnimation, SpriteSheet, Texture, TextureHandle, TextureSize},
};

/// A 32×16 PNG with four 8×16 opaque colour cells: red, cyan, yellow, blue.
const DEMO_ATLAS_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 32, 0, 0, 0, 16, 8, 6,
    0, 0, 0, 119, 0, 125, 89, 0, 0, 0, 50, 73, 68, 65, 84, 120, 218, 99, 120, 233, 154, 240, 31,
    31, 230, 184, 121, 13, 47, 254, 127, 49, 13, 47, 78, 171, 123, 133, 23, 51, 140, 58, 96, 212,
    1, 163, 14, 24, 117, 192, 168, 3, 70, 29, 48, 234, 128, 129, 118, 0, 0, 53, 115, 162, 204, 4,
    161, 251, 150, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

const GRID: [u32; 2] = [24, 16];
const TILE: f32 = 32.0;
const MAP_CENTER: [f32; 2] = [384.0, 256.0];
const PLAYER_SIZE: [f32; 2] = [28.0, 40.0];

struct Demo {
    _textures: Assets<Texture>,
    texture: TextureHandle,
    profile: Game2dProfile,
    playable: PlayableLoop2d,
    facing: VelocityFacingPolicy2d,
    attack_pressed: bool,
}

fn main() -> Result<(), yuyib::app::ApplicationError> {
    let (mut demo, image) = create_demo();
    demo.profile
        .queue_texture(demo.texture, image)
        .expect("atlas fits the texture queue");
    let demo = Rc::new(RefCell::new(demo));
    let event_demo = Rc::clone(&demo);
    let update_demo = Rc::clone(&demo);
    let render_demo = Rc::clone(&demo);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — animator set / SM / facing (WASD, Space=attack)".to_owned(),
            width: 960,
            height: 540,
            resizable: true,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.02, 0.03, 0.05, 1.0))
        .render_loop(RenderLoop::Continuous)
        .on_window_event(move |event, _context| {
            let mut demo = event_demo.borrow_mut();
            demo.playable.handle_window_event(event);
            if is_space_press(event) {
                demo.attack_pressed = true;
            }
        })
        .on_frame(move |context| {
            let mut demo = update_demo.borrow_mut();
            let delta = context.frame().delta;
            let Demo {
                profile,
                playable,
                facing,
                attack_pressed,
                ..
            } = &mut *demo;
            let actor = playable.actor();
            if *attack_pressed {
                *attack_pressed = false;
                if let Some(mut animator) = profile.world_mut().get_mut::<SpriteAnimator2d>(actor) {
                    let _ = animator.play_restart("attack");
                }
            }
            let axis = playable.movement().axis();
            apply_velocity_facing_2d(profile.world_mut(), actor, [axis.x, axis.y], facing)
                .expect("actor owns animator + sprite");
            playable
                .step(profile, delta)
                .expect("demo player starts outside solids");
        })
        .on_render(move |frame| {
            let mut demo = render_demo.borrow_mut();
            let Demo {
                profile, playable, ..
            } = &mut *demo;
            let _stats = playable
                .render(profile, frame)
                .expect("authored scene remains valid");
        })
        .run()
}

fn is_space_press(event: &winit::event::WindowEvent) -> bool {
    use winit::{
        event::{ElementState, WindowEvent},
        keyboard::{KeyCode, PhysicalKey},
    };
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    matches!(
        (event.physical_key, event.state, event.repeat),
        (PhysicalKey::Code(KeyCode::Space), ElementState::Pressed, false)
    )
}

fn create_demo() -> (Demo, DecodedImage) {
    let image = decode_bytes(DEMO_ATLAS_PNG, DecodePolicy::default())
        .expect("embedded PNG is valid");
    let texture_size = image.texture().size();
    let mut textures = Assets::new();
    let texture = textures.insert(image.texture().clone());
    let cell = TextureSize::new(8, 16).expect("non-empty atlas cell");
    let sheet =
        SpriteSheet::from_grid(texture, texture_size, cell).expect("regular four-cell atlas");
    let idle_regions = [
        sheet.region(0).expect("cell 0"),
        sheet.region(0).expect("cell 0"),
    ];
    let walk_regions = [
        sheet.region(0).expect("cell 0"),
        sheet.region(1).expect("cell 1"),
        sheet.region(2).expect("cell 2"),
        sheet.region(3).expect("cell 3"),
    ];
    let attack_regions = [
        sheet.region(3).expect("cell 3"),
        sheet.region(2).expect("cell 2"),
    ];
    let idle = SpriteAnimation::from_regions(
        &idle_regions,
        Duration::from_millis(220),
        PlaybackMode::Loop,
    )
    .expect("idle");
    let walk = SpriteAnimation::from_regions(
        &walk_regions,
        Duration::from_millis(110),
        PlaybackMode::PingPong,
    )
    .expect("walk");
    let attack = SpriteAnimation::from_regions(
        &attack_regions,
        Duration::from_millis(90),
        PlaybackMode::Once,
    )
    .expect("attack");
    let wall = sheet.region(2).expect("wall cell");
    let mut profile = Game2dProfile::new(Game2dSceneConfig::default());

    let cells = vec![Some(0); (GRID[0] * GRID[1]) as usize];
    let mut solid = vec![false; cells.len()];
    for row in 0..GRID[1] {
        for column in 0..GRID[0] {
            if row == 0 || row + 1 == GRID[1] || column == 0 || column + 1 == GRID[0] {
                solid[(row * GRID[0] + column) as usize] = true;
            }
        }
    }
    profile.world_mut().spawn((
        TileMap2d::new(GRID, [TILE, TILE], vec![wall], cells)
            .expect("complete map data")
            .with_layer(0),
        TileCollision2d::new(GRID, solid).expect("collision grid matches tile grid"),
    ));

    let set = AnimationSet::new()
        .with("idle", idle.clone())
        .with("walk", walk)
        .with("attack", attack);
    let machine = AnimationStateMachine::new("idle")
        .expect("idle")
        .with_clip("walk")
        .expect("walk")
        .with_state(
            "attack",
            AnimationStateDef::clip("attack").on_finished("idle"),
        )
        .expect("attack");

    let player = profile
        .world_mut()
        .spawn((
            Sprite2d::new(sheet.region(0).expect("atlas frame"))
                .with_position(MAP_CENTER)
                .with_size(PLAYER_SIZE)
                .with_layer(10),
            AnimatedSprite2d::new(idle),
            SpriteAnimator2d::new(set, machine).expect("clips match machine"),
            KinematicSpriteController2d::new([PLAYER_SIZE[0] * 0.5, PLAYER_SIZE[1] * 0.5], 220.0)
                .expect("finite controller"),
        ))
        .id();
    let playable = PlayableLoop2d::new(
        PlayableLoopDesc2d::new(player, 256).expect("fixed positive tile budget"),
    );

    (
        Demo {
            _textures: textures,
            texture,
            profile,
            playable,
            facing: VelocityFacingPolicy2d::new("idle", "walk"),
            attack_pressed: false,
        },
        image,
    )
}
