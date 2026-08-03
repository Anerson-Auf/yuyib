//! Deep 2D thin UI: [`PlayableLoop2d`] + pause overlay via [`ApplicationUi`].
//!
//! Escape toggles pause. While paused the world freezes (`step` skipped) but
//! still renders under a dim overlay with title + resume hint. The overlay uses
//! [`pause_overlay_tree`] + [`ApplicationUi::with_active_flag`].
//!
//! ```text
//! cargo run -p yuyib --example two_d_playable_hud --features "app,two-d,ui"
//! ```
//!
//! Controls: WASD / arrows move, Escape pause/resume.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use yuyib::{
    app::{Application, ApplicationUi, NativeUiTextConfig, RenderLoop, pause_overlay_tree},
    assets::Assets,
    game_2d::{
        AnimatedSprite2d, Game2dSceneConfig, KinematicSpriteController2d, Sprite2d,
        TileCollision2d, TileMap2d,
    },
    image::{DecodePolicy, DecodedImage, decode_bytes},
    platform::{WindowConfig, winit},
    profile_2d::{Game2dProfile, PlayableLoop2d, PlayableLoopDesc2d},
    render::ClearColor,
    two_d::{PlaybackMode, SpriteSheet, Texture, TextureHandle, TextureSize},
    ui_text::FontSource,
};

/// A 32×16 PNG with four 8×16 opaque colour cells: red, cyan, yellow, blue.
const DEMO_ATLAS_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 32, 0, 0, 0, 16, 8, 6,
    0, 0, 0, 119, 0, 125, 89, 0, 0, 0, 50, 73, 68, 65, 84, 120, 218, 99, 120, 233, 154, 240, 31,
    31, 230, 184, 121, 13, 47, 254, 127, 49, 13, 47, 78, 171, 123, 133, 23, 51, 140, 58, 96, 212,
    1, 163, 14, 24, 117, 192, 168, 3, 70, 29, 48, 234, 128, 129, 118, 0, 0, 53, 115, 162, 204, 4,
    161, 251, 150, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

const DEV_FONT_FILE: &str = r"C:\Windows\Fonts\segoeui.ttf";
const GRID: [u32; 2] = [30, 20];
const TILE: f32 = 32.0;
const MAP_CENTER: [f32; 2] = [480.0, 320.0];
const PLAYER_SIZE: [f32; 2] = [28.0, 40.0];

struct Playground {
    _textures: Assets<Texture>,
    texture: TextureHandle,
    profile: Game2dProfile,
    playable: PlayableLoop2d,
    paused: Rc<Cell<bool>>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut playground, image) = create_playground();
    playground
        .profile
        .queue_texture(playground.texture, image)
        .expect("atlas fits the texture queue");
    let ui_active = Rc::clone(&playground.paused);
    let playground = Rc::new(RefCell::new(playground));
    let event_playground = Rc::clone(&playground);
    let update_playground = Rc::clone(&playground);
    let render_playground = Rc::clone(&playground);

    let ui = ApplicationUi::new(pause_overlay_tree(
        "Paused",
        "Esc — resume · WASD moves while playing",
    )?)
    .with_text(NativeUiTextConfig::new(FontSource::file(DEV_FONT_FILE)))?
    .with_active_flag(ui_active);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — 2D playable HUD (Esc pause)".to_owned(),
            width: 960,
            height: 540,
            resizable: true,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.015, 0.025, 0.045, 1.0))
        .render_loop(RenderLoop::Continuous)
        .on_window_event(move |event, _context| {
            let mut playground = event_playground.borrow_mut();
            if is_escape_press(event) {
                let next = !playground.paused.get();
                playground.paused.set(next);
                return;
            }
            if !playground.paused.get() {
                playground.playable.handle_window_event(event);
            }
        })
        .on_frame(move |context| {
            let mut playground = update_playground.borrow_mut();
            if playground.paused.get() {
                return;
            }
            let Playground {
                profile, playable, ..
            } = &mut *playground;
            playable
                .step(profile, context.frame().delta)
                .expect("demo player stays outside solids");
        })
        .on_render(move |frame| {
            let mut playground = render_playground.borrow_mut();
            let Playground {
                profile, playable, ..
            } = &mut *playground;
            let _stats = playable
                .render(profile, frame)
                .expect("authored atlas remains valid");
        })
        .ui(ui)
        .run()?;
    Ok(())
}

fn is_escape_press(event: &winit::event::WindowEvent) -> bool {
    use winit::{
        event::{ElementState, WindowEvent},
        keyboard::{KeyCode, PhysicalKey},
    };
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return false;
    };
    matches!(
        (&event.physical_key, event.state, event.repeat),
        (PhysicalKey::Code(KeyCode::Escape), ElementState::Pressed, false)
    )
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
    let wall = sheet.region(2).expect("atlas has third cell");
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
    let player = profile
        .world_mut()
        .spawn((
            Sprite2d::new(sheet.region(0).expect("atlas frame"))
                .with_position(MAP_CENTER)
                .with_size(PLAYER_SIZE)
                .with_layer(10),
            AnimatedSprite2d::new(player_animation),
            KinematicSpriteController2d::new([PLAYER_SIZE[0] * 0.5, PLAYER_SIZE[1] * 0.5], 220.0)
                .expect("finite controller"),
        ))
        .id();
    let playable = PlayableLoop2d::new(
        PlayableLoopDesc2d::new(player, 256).expect("fixed positive tile budget"),
    );

    (
        Playground {
            _textures: textures,
            texture,
            profile,
            playable,
            paused: Rc::new(Cell::new(false)),
        },
        image,
    )
}
