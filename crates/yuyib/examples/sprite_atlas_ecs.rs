//! A self-contained 2D atlas rendered from ECS components.
//!
//! Run from the workspace root with:
//!
//! ```text
//! cargo run -p yuyib --example sprite_atlas_ecs
//! ```
//!
//! The example embeds a four-cell PNG atlas, decodes it through `yuyib-image`,
//! stores its metadata in `Assets`, uploads it once, and extracts `Sprite2d`
//! components on every render frame. No asset files are required.

use std::{cell::RefCell, rc::Rc};

use yuyib::{
    app::{Application, RenderLoop},
    assets::Assets,
    ecs::prelude::*,
    game_2d::{Sprite2d, extract_sprites},
    image::{DecodePolicy, DecodedImage, decode_bytes},
    platform::WindowConfig,
    render::{ClearColor, RenderFrame},
    render_2d::{Camera2d, GpuSpriteTexture, SpriteRenderer},
    two_d::{PixelPoint, Texture, TextureHandle, TextureRegion, TextureSize},
};

/// A 32×16 PNG with four 8×16 opaque colour cells: red, cyan, yellow, blue.
const DEMO_ATLAS_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 32, 0, 0, 0, 16, 8, 6,
    0, 0, 0, 119, 0, 125, 89, 0, 0, 0, 50, 73, 68, 65, 84, 120, 218, 99, 120, 233, 154, 240, 31,
    31, 230, 184, 121, 13, 47, 254, 127, 49, 13, 47, 78, 171, 123, 133, 23, 51, 140, 58, 96, 212,
    1, 163, 14, 24, 117, 192, 168, 3, 70, 29, 48, 234, 128, 129, 118, 0, 0, 53, 115, 162, 204, 4,
    161, 251, 150, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

struct AtlasScene {
    texture: TextureHandle,
    texture_size: TextureSize,
    gpu_texture: GpuSpriteTexture,
    sprite_renderer: SpriteRenderer,
}

struct DemoWorld {
    _textures: Assets<Texture>,
    image: DecodedImage,
    world: World,
    texture: TextureHandle,
}

impl AtlasScene {
    fn new(frame: &RenderFrame<'_>, demo: &DemoWorld) -> Self {
        let texture_size = demo.image.texture().size();
        let sprite_renderer = SpriteRenderer::new_for_frame(frame);
        let gpu_texture = sprite_renderer
            .upload_rgba8_for_frame(frame, demo.texture, &demo.image)
            .expect("the embedded 32×16 RGBA8 atlas fits every supported WGPU device");

        Self {
            texture: demo.texture,
            texture_size,
            gpu_texture,
            sprite_renderer,
        }
    }

    fn render(&mut self, frame: &mut RenderFrame<'_>, world: &mut World) {
        let extracted = extract_sprites(world);
        for batch in extracted.batches() {
            let prepared = self
                .sprite_renderer
                .prepare(
                    batch.texture(),
                    self.texture_size,
                    batch.draws().iter().copied(),
                )
                .expect("every demo sprite uses a validated region from the atlas");
            assert_eq!(batch.texture(), self.texture);
            self.sprite_renderer
                .draw(frame, Camera2d::default(), &self.gpu_texture, &prepared)
                .expect("the validated demo camera and GPU texture must render");
        }
    }
}

fn main() -> Result<(), yuyib::app::ApplicationError> {
    let world = Rc::new(RefCell::new(create_world()));
    let update_world = Rc::clone(&world);
    let render_world = Rc::clone(&world);
    let scene = Rc::new(RefCell::new(None));
    let render_scene = Rc::clone(&scene);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — ECS sprite atlas".to_owned(),
            width: 960,
            height: 540,
            resizable: true,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.025, 0.035, 0.06, 1.0))
        .render_loop(RenderLoop::Continuous)
        .on_frame(move |context| {
            let delta_seconds = context.frame().delta.as_secs_f32();
            let mut world = update_world.borrow_mut();
            let mut sprites = world.world.query::<&mut Sprite2d>();
            for mut sprite in sprites.iter_mut(&mut world.world) {
                sprite.rotation_radians += delta_seconds;
            }
        })
        .on_render(move |frame| {
            if render_scene.borrow().is_none() {
                let world = render_world.borrow();
                *render_scene.borrow_mut() = Some(AtlasScene::new(frame, &world));
            }
            let mut scene = render_scene.borrow_mut();
            let scene = scene
                .as_mut()
                .expect("the atlas scene is initialised before rendering");
            scene.render(frame, &mut render_world.borrow_mut().world);
        })
        .run()
}

fn decode_demo_atlas() -> DecodedImage {
    decode_bytes(DEMO_ATLAS_PNG, DecodePolicy::default())
        .expect("the embedded atlas is a valid PNG within the default decode budget")
}

fn create_world() -> DemoWorld {
    let image = decode_demo_atlas();
    let texture_size = image.texture().size();
    let mut textures = Assets::new();
    let texture = textures.insert(image.texture().clone());
    let cell = TextureSize::new(8, 16).expect("atlas cells are non-empty");
    let mut world = World::new();

    for (layer, (origin_x, position, rotation)) in [
        (0, [-240.0, 0.0], 0.0),
        (8, [-80.0, 0.0], 0.25),
        (16, [80.0, 0.0], 0.5),
        (24, [240.0, 0.0], 0.75),
    ]
    .into_iter()
    .enumerate()
    {
        let region = TextureRegion::new(
            texture,
            texture_size,
            PixelPoint { x: origin_x, y: 0 },
            cell,
        )
        .expect("each cell fits the demo atlas");
        world.spawn(
            Sprite2d::new(region)
                .with_position(position)
                .with_size([112.0, 224.0])
                .with_rotation(rotation)
                .with_layer(i32::try_from(layer).expect("four layers fit i32")),
        );
    }

    DemoWorld {
        _textures: textures,
        image,
        world,
        texture,
    }
}
