//! Minimal high-level 3D ECS scene: one model, hierarchy-ready transform and ECS light.
//!
//! Run with `cargo run -p yuyib --example game_3d_scene`.

use std::{cell::RefCell, rc::Rc};

use yuyib::{
    app::{Application, RenderLoop},
    assets::Assets,
    ecs::prelude::World,
    game_3d::{DirectionalLight3d, Model3d, Transform3d},
    model::Model,
    platform::WindowConfig,
    render::{ClearColor, ColorPostProcess},
    render_3d::{Game3dScene, Game3dSceneConfig, Game3dShading},
};

fn main() -> Result<(), yuyib::app::ApplicationError> {
    let mut models = Assets::new();
    let cube = models.insert(Model::cube(0.75).expect("the built-in cube is valid"));
    let mut world = World::new();
    let cube_entity = world
        .spawn((Model3d::new(cube), Transform3d::default()))
        .id();
    world.spawn(
        DirectionalLight3d::new([-0.35, -1.0, -0.45], [1.0, 0.95, 0.88], 0.9)
            .expect("the demo light is finite"),
    );

    let models = Rc::new(models);
    let world = Rc::new(RefCell::new(world));
    let angle = Rc::new(RefCell::new(0.0_f32));
    let scene = Rc::new(RefCell::new(
        Game3dScene::new(
            ".",
            Game3dSceneConfig::default().with_shading(Game3dShading::Pbr),
        )
        .expect("the current project directory is a valid texture root"),
    ));

    let update_world = Rc::clone(&world);
    let update_angle = Rc::clone(&angle);
    let render_world = Rc::clone(&world);
    let render_scene = Rc::clone(&scene);
    let render_models = Rc::clone(&models);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — high-level Game3dScene PBR".to_owned(),
            width: 960,
            height: 540,
            ..Default::default()
        })
        .render_loop(RenderLoop::Continuous)
        .clear_color(ClearColor::linear(0.015, 0.025, 0.05, 1.0))
        .color_post_process(
            ColorPostProcess::filmic()
                .with_exposure_ev(0.5)
                .expect("the example exposure is within renderer limits"),
        )
        .on_frame(move |context| {
            let mut angle = update_angle.borrow_mut();
            *angle += context.frame().delta.as_secs_f32() * 0.65;
            let half = *angle * 0.5;
            update_world
                .borrow_mut()
                .get_mut::<Transform3d>(cube_entity)
                .expect("the cube lives for the example")
                .rotation = [0.0, half.sin(), 0.0, half.cos()];
        })
        .on_render(move |frame| {
            let _stats = render_scene
                .borrow_mut()
                .render(
                    frame,
                    &mut render_world.borrow_mut(),
                    render_models.as_ref(),
                )
                .expect("the built-in scene has supported standard materials");
        })
        .run()
}
