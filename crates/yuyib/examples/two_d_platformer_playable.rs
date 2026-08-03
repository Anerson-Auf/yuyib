//! HL 2D platformer: [`PlatformerPlayable2d`] + [`Game2dProfile`].
//!
//! Demonstrates Rapier gravity/jump/walls/one-way platform with sprite sync and
//! camera follow. Physics is Y-up metres; sprites are Y-down pixels via
//! [`physics_to_sprite`].
//!
//! ```text
//! cargo run -p yuyib --example two_d_platformer_playable --features "two-d,character-2d"
//! ```
//!
//! Controls: A/D or arrows move, Space jumps.

use std::{cell::RefCell, rc::Rc, time::Duration};

use yuyib::{
    app::{Application, RenderLoop},
    assets::Assets,
    character_2d::PlatformerControllerConfig2d,
    game_2d::{AnimatedSprite2d, Game2dSceneConfig, Sprite2d},
    image::{DecodePolicy, DecodedImage, decode_bytes},
    platform::WindowConfig,
    profile_2d::{
        Game2dProfile, PlatformerPlayable2d, PlatformerPlayableDesc2d, physics_to_sprite,
    },
    render::ClearColor,
    two_d::{PlaybackMode, SpriteSheet, Texture, TextureHandle, TextureSize},
};

/// A 32×16 PNG with four 8×16 opaque colour cells: red, cyan, yellow, blue.
const DEMO_ATLAS_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 32, 0, 0, 0, 16, 8, 6,
    0, 0, 0, 119, 0, 125, 89, 0, 0, 0, 50, 73, 68, 65, 84, 120, 218, 99, 120, 233, 154, 240, 31,
    31, 230, 184, 121, 13, 47, 254, 127, 49, 13, 47, 78, 171, 123, 133, 23, 51, 140, 58, 96, 212,
    1, 163, 14, 24, 117, 192, 168, 3, 70, 29, 48, 234, 128, 129, 118, 0, 0, 53, 115, 162, 204, 4,
    161, 251, 150, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

const PPU: f32 = 32.0;
const SPAWN_PHYSICS: [f32; 2] = [0.0, 3.0];

struct Playground {
    _textures: Assets<Texture>,
    texture: TextureHandle,
    profile: Game2dProfile,
    playable: PlatformerPlayable2d,
}

fn main() -> Result<(), yuyib::app::ApplicationError> {
    let (mut playground, image) = create_playground();
    playground
        .profile
        .queue_texture(playground.texture, image)
        .expect("atlas fits the texture queue");
    let playground = Rc::new(RefCell::new(playground));
    let event_playground = Rc::clone(&playground);
    let update_playground = Rc::clone(&playground);
    let render_playground = Rc::clone(&playground);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — 2D platformer HL (A/D + Space)".to_owned(),
            width: 960,
            height: 540,
            resizable: true,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.02, 0.03, 0.06, 1.0))
        .render_loop(RenderLoop::Continuous)
        .on_window_event(move |event, _context| {
            event_playground
                .borrow_mut()
                .playable
                .handle_window_event(event);
        })
        .on_frame(move |context| {
            let mut playground = update_playground.borrow_mut();
            let Playground {
                profile, playable, ..
            } = &mut *playground;
            playable
                .step(profile, context.frame().delta)
                .expect("platformer step stays valid for the authored level");
        })
        .on_render(move |frame| {
            let mut playground = render_playground.borrow_mut();
            let Playground {
                profile, playable, ..
            } = &mut *playground;
            let _stats = playable
                .render(profile, frame)
                .expect("authored atlas and solids remain valid");
        })
        .run()
}

fn create_playground() -> (Playground, DecodedImage) {
    let image = decode_bytes(DEMO_ATLAS_PNG, DecodePolicy::default())
        .expect("embedded PNG is valid");
    let texture_size = image.texture().size();
    let mut textures = Assets::new();
    let texture = textures.insert(image.texture().clone());
    let cell = TextureSize::new(8, 16).expect("non-empty atlas cell");
    let sheet =
        SpriteSheet::from_grid(texture, texture_size, cell).expect("regular four-cell atlas");
    let player_animation = sheet
        .animation(Duration::from_millis(140), PlaybackMode::PingPong)
        .expect("four atlas cells make an animation");
    let ground_region = sheet.region(2).expect("ground atlas cell");
    let player_region = sheet.region(0).expect("player atlas cell");
    let accent_region = sheet.region(1).expect("accent atlas cell");

    let mut profile = Game2dProfile::new(Game2dSceneConfig::default());
    spawn_solid_sprite(&mut profile, ground_region, [0.0, -0.5], [16.0, 1.0], 0);
    spawn_solid_sprite(&mut profile, ground_region, [3.0, 1.0], [0.5, 3.0], 0);
    spawn_solid_sprite(&mut profile, accent_region, [0.0, 2.5], [3.0, 0.2], 1);

    let player = profile
        .world_mut()
        .spawn((
            Sprite2d::new(player_region)
                .with_position(physics_to_sprite(SPAWN_PHYSICS, PPU))
                .with_size([PPU * 0.5, PPU * 1.2])
                .with_layer(10),
            AnimatedSprite2d::new(player_animation),
        ))
        .id();

    let mut playable = PlatformerPlayable2d::spawn(
        PlatformerPlayableDesc2d::new(player, SPAWN_PHYSICS)
            .expect("finite spawn / scale")
            .with_pixels_per_unit(PPU)
            .with_config(PlatformerControllerConfig2d {
                jump_speed: 16.0,
                ..PlatformerControllerConfig2d::default()
            }),
    )
    .expect("platformer spawn");
    let dynamics = playable.dynamics_mut();
    let _ground = dynamics
        .insert_fixed_cuboid([0.0, -0.5], [8.0, 0.5])
        .expect("ground");
    let _wall = dynamics
        .insert_fixed_cuboid([3.0, 1.0], [0.25, 1.5])
        .expect("wall");
    let _one_way = dynamics
        .insert_one_way_platform_cuboid([0.0, 2.5], [1.5, 0.1])
        .expect("one-way");

    (
        Playground {
            _textures: textures,
            texture,
            profile,
            playable,
        },
        image,
    )
}

fn spawn_solid_sprite(
    profile: &mut Game2dProfile,
    region: yuyib::two_d::TextureRegion,
    physics_center: [f32; 2],
    physics_size: [f32; 2],
    layer: i32,
) {
    let position = physics_to_sprite(physics_center, PPU);
    let size = [physics_size[0] * PPU, physics_size[1] * PPU];
    profile.world_mut().spawn(
        Sprite2d::new(region)
            .with_position(position)
            .with_size(size)
            .with_layer(layer),
    );
}
