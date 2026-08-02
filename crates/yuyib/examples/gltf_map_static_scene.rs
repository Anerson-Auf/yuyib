//! Native 3D viewer for the real static map/model fixtures.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p yuyib --example gltf_map_static_scene
//! ```
//!
//! An allow-listed alternate fixture can be supplied after `--`, for example
//! `cargo run -p yuyib --example gltf_map_static_scene -- cyber_samurai.glb`.
//!
//! The map is imported from `for_tests`, spawned through its original glTF
//! hierarchy (including affine matrix nodes), and rendered with the source
//! double-sided rasterization mode. A procedural cube marks the calculated
//! scene centre. Базовые текстуры карты загружаются автоматически; normal map,
//! PBR и прозрачность пока намеренно не входят в этот пример. Управление:
//! `WASD` — движение по горизонтали, `Space`/`Ctrl` — вверх/вниз, `Shift` —
//! ускорение, мышь — осмотр, `Esc` — выход. Курсор скрыт и удерживается в
//! окне, как в обычной игре.

use std::{cell::RefCell, error::Error, path::Path, rc::Rc};

use yuyib::{
    app::{Application, RenderLoop},
    assets::Assets,
    ecs::prelude::World,
    game_3d::{Model3d, SceneBoundsResult3d, Transform3d, extract_models, scene_bounds_3d},
    gltf::{ImportOptions, import_scene_path_with_options},
    input::{FreeCameraConfig3d, FreeCameraController3d},
    model::Model,
    model_assets::ModelTextureLoader,
    platform::WindowConfig,
    render::ClearColor,
    render_3d::BaseColorSceneRenderer3d,
    scene::{SceneSelection, spawn_scene},
};

#[allow(
    clippy::too_many_lines,
    reason = "The runnable viewer keeps fixture selection, scene setup and application callbacks together."
)]
fn main() -> Result<(), Box<dyn Error>> {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../for_tests");
    let fixture_name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "no_i_am_not_a_human_location__map.glb".to_owned());
    let import_options = match fixture_name.as_str() {
        "no_i_am_not_a_human_location__map.glb"
        | "the_billiards_room.glb"
        | "the_parade_shield_of_king_erik_xiv_of_sweden.glb" => ImportOptions::default(),
        "cyber_samurai.glb" => ImportOptions::skeletal_preview(),
        _ => {
            return Err(format!(
                "unsupported fixture {fixture_name:?}; use the map, billiards room, shield or cyber samurai GLB"
            )
            .into());
        }
    };
    let texture_loader = ModelTextureLoader::new(&fixture_root)?;
    let imported =
        import_scene_path_with_options(fixture_root.join(&fixture_name), import_options)?;
    if !imported.report().is_complete() {
        eprintln!(
            "Static preview skipped {} non-triangle helper primitive(s).",
            imported.report().skipped_primitive_count()
        );
    }
    let mut world = World::new();
    let mut models = Assets::<Model>::new();
    let _map = spawn_scene(&mut world, &mut models, &imported, SceneSelection::Default)?;

    let (centre, radius) = match scene_bounds_3d(&mut world, &models)? {
        SceneBoundsResult3d::Bounds(bounds) => (bounds.centre(), bounds.radius().max(10.0)),
        SceneBoundsResult3d::Empty => ([0.0; 3], 10.0),
    };
    let marker = models.insert(Model::cube((radius * 0.0125).clamp(0.5, 8.0))?);
    world.spawn((Model3d::new(marker), Transform3d::from_translation(centre)));

    let camera_config = FreeCameraConfig3d {
        move_speed: (radius * 0.3).clamp(2.0, 150.0),
        near: (radius * 0.001).max(0.01),
        far: radius * 8.0,
        ..FreeCameraConfig3d::default()
    };
    let camera = Rc::new(RefCell::new(FreeCameraController3d::looking_at(
        camera_config,
        [
            centre[0] + radius * 1.25,
            centre[1] + radius * 0.8,
            centre[2] + radius * 1.25,
        ],
        centre,
    )?));
    let initial_cursor = camera.borrow().initial_cursor_control();

    let shared_world = Rc::new(RefCell::new(world));
    let shared_models = Rc::new(models);
    let render_world = Rc::clone(&shared_world);
    let render_models = Rc::clone(&shared_models);
    let render_scene = Rc::new(RefCell::new(None::<BaseColorSceneRenderer3d>));
    let render_scene_state = Rc::clone(&render_scene);
    let window_camera = Rc::clone(&camera);
    let device_camera = Rc::clone(&camera);
    let update_camera = Rc::clone(&camera);
    let render_camera = Rc::clone(&camera);

    Application::new()
        .window(WindowConfig {
            title: format!("Yuyib — {fixture_name}"),
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.025, 0.035, 0.06, 1.0))
        .render_loop(RenderLoop::Continuous)
        .cursor_control(initial_cursor)
        .on_window_event(move |event, context| {
            let result = window_camera.borrow_mut().handle_window_event(event);
            if let Some(cursor) = result.cursor_control {
                context.set_cursor_control(cursor);
            }
            if result.exit_requested {
                context.request_exit();
            }
        })
        .on_device_event(move |event, _context| {
            let _ = device_camera.borrow_mut().handle_device_event(event);
        })
        .on_frame(move |context| {
            update_camera
                .borrow_mut()
                .step(context.frame().delta.as_secs_f32())
                .expect("проверенная камера принимает время кадра приложения");
        })
        .on_render(move |frame| {
            let mut bridge = render_scene_state.borrow_mut();
            let bridge = bridge.get_or_insert_with(|| {
                BaseColorSceneRenderer3d::new_for_frame(frame, texture_loader.clone())
            });
            let extracted = {
                let mut world = render_world.borrow_mut();
                extract_models(&mut world)
            };
            if let Err(error) = bridge.draw_for_frame(
                frame,
                render_camera.borrow().camera(),
                &render_models,
                &extracted,
            ) {
                eprintln!("3D map render failed: {error}");
            }
        })
        .run()?;
    Ok(())
}
