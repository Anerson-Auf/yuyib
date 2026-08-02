//! Небольшая 2D-сцена: sprite sheet, animation, camera follow и столкновения.
//!
//! Запуск из корня workspace:
//!
//! ```text
//! cargo run -p yuyib --example two_d_tile_playground
//! ```
//!
//! Управление: WASD или стрелки. Персонаж не проходит через границу комнаты.
//! Пример использует один встроенный atlas, поэтому не требует файлов рядом с
//! executable. В настоящей игре замените `decode_demo_atlas` асинхронной
//! загрузкой через `AssetLoader`.

use std::{cell::RefCell, rc::Rc, time::Duration};

use yuyib::{
    app::{Application, RenderLoop},
    assets::Assets,
    ecs::prelude::{Entity, World},
    game_2d::{
        AnimatedSprite2d, Game2dScene, KinematicSpriteController2d, Sprite2d, SpriteMoveInput2d,
        TileCollision2d, TileKinematicAabbLimits2d, TileMap2d, step_kinematic_sprite_controller_2d,
        step_sprite_animations_2d,
    },
    image::{DecodePolicy, DecodedImage, decode_bytes},
    platform::{WindowConfig, winit},
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

const GRID: [u32; 2] = [30, 20];
const TILE: f32 = 32.0;
const MAP_CENTER: [f32; 2] = [480.0, 320.0];
const PLAYER_SIZE: [f32; 2] = [28.0, 40.0];

struct Playground {
    _textures: Assets<Texture>,
    texture: TextureHandle,
    world: World,
    player: Entity,
    input: HeldInput,
}

#[derive(Default)]
struct HeldInput {
    horizontal: HeldAxis,
    vertical: HeldAxis,
}

#[derive(Default)]
struct HeldAxis {
    negative: bool,
    positive: bool,
}

impl HeldInput {
    fn handle(&mut self, event: &winit::event::WindowEvent) {
        use winit::{
            event::{ElementState, WindowEvent},
            keyboard::{KeyCode, PhysicalKey},
        };
        let WindowEvent::KeyboardInput { event, .. } = event else {
            return;
        };
        let PhysicalKey::Code(key) = event.physical_key else {
            return;
        };
        let held = event.state == ElementState::Pressed;
        match key {
            KeyCode::KeyA | KeyCode::ArrowLeft => self.horizontal.negative = held,
            KeyCode::KeyD | KeyCode::ArrowRight => self.horizontal.positive = held,
            KeyCode::KeyW | KeyCode::ArrowUp => self.vertical.negative = held,
            KeyCode::KeyS | KeyCode::ArrowDown => self.vertical.positive = held,
            _ => {}
        }
    }

    fn movement(&self) -> SpriteMoveInput2d {
        SpriteMoveInput2d::new([
            (if self.horizontal.positive { 1.0 } else { 0.0 })
                - (if self.horizontal.negative { 1.0 } else { 0.0 }),
            (if self.vertical.positive { 1.0 } else { 0.0 })
                - (if self.vertical.negative { 1.0 } else { 0.0 }),
        ])
        .expect("booleans always produce finite input")
    }
}

fn main() -> Result<(), yuyib::app::ApplicationError> {
    let (playground, image) = create_playground();
    let mut scene = Game2dScene::default();
    scene
        .queue_texture(playground.texture, image)
        .expect("the scene texture queue has room for the compact atlas");
    let playground = Rc::new(RefCell::new(playground));
    let event_playground = Rc::clone(&playground);
    let update_playground = Rc::clone(&playground);
    let render_playground = Rc::clone(&playground);
    let scene = Rc::new(RefCell::new(scene));
    let render_scene = Rc::clone(&scene);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — 2D playground (WASD / arrows)".to_owned(),
            width: 960,
            height: 540,
            resizable: true,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.015, 0.025, 0.045, 1.0))
        .render_loop(RenderLoop::Continuous)
        .on_window_event(move |event, _context| {
            event_playground.borrow_mut().input.handle(event);
        })
        .on_frame(move |context| {
            let mut playground = update_playground.borrow_mut();
            let delta = context.frame().delta.min(Duration::from_millis(50));
            let input = playground.input.movement();
            let player = playground.player;
            step_kinematic_sprite_controller_2d(
                &mut playground.world,
                player,
                input,
                delta,
                TileKinematicAabbLimits2d::new(256).expect("fixed positive tile budget"),
            )
            .expect("the demo player begins outside solids and map data is valid");
            let _events = step_sprite_animations_2d(&mut playground.world, delta);
        })
        .on_render(move |frame| {
            let mut playground = render_playground.borrow_mut();
            let player_position = playground
                .world
                .get::<Sprite2d>(playground.player)
                .expect("player lives for the whole example")
                .position;
            let mut scene = render_scene.borrow_mut();
            scene.camera_mut().position = player_position;
            let _stats = scene
                .render(frame, &mut playground.world)
                .expect("authored scene and embedded atlas remain valid");
        })
        .run()
}

fn create_playground() -> (Playground, DecodedImage) {
    let image = decode_bytes(DEMO_ATLAS_PNG, DecodePolicy::default())
        .expect("the embedded PNG is valid and stays within decode limits");
    let texture_size = image.texture().size();
    let mut textures = Assets::new();
    let texture = textures.insert(image.texture().clone());
    let cell = TextureSize::new(8, 16).expect("non-empty atlas cell");
    let sheet =
        SpriteSheet::from_grid(texture, texture_size, cell).expect("regular four-cell atlas");
    let player_animation = sheet
        .animation(Duration::from_millis(140), PlaybackMode::PingPong)
        .expect("four atlas cells make an animation");
    let wall = sheet.region(2).expect("atlas has third cell");
    let mut world = World::new();

    let cells = vec![Some(0); (GRID[0] * GRID[1]) as usize];
    let mut solid = vec![false; cells.len()];
    for row in 0..GRID[1] {
        for column in 0..GRID[0] {
            if row == 0 || row + 1 == GRID[1] || column == 0 || column + 1 == GRID[0] {
                solid[(row * GRID[0] + column) as usize] = true;
            }
        }
    }
    world.spawn((
        TileMap2d::new(GRID, [TILE, TILE], vec![wall], cells)
            .expect("complete map data")
            .with_layer(0),
        TileCollision2d::new(GRID, solid).expect("collision grid matches tile grid"),
    ));
    let player = world
        .spawn((
            Sprite2d::new(sheet.region(0).expect("atlas frame"))
                .with_position(MAP_CENTER)
                .with_size(PLAYER_SIZE)
                .with_layer(10),
            AnimatedSprite2d::new(player_animation),
            // Collision half-extents match the visible 28×40 sprite exactly;
            // a larger controller creates a visible air gap at walls.
            KinematicSpriteController2d::new([PLAYER_SIZE[0] * 0.5, PLAYER_SIZE[1] * 0.5], 220.0)
                .expect("finite controller"),
        ))
        .id();

    (
        Playground {
            _textures: textures,
            texture,
            world,
            player,
            input: HeldInput::default(),
        },
        image,
    )
}
