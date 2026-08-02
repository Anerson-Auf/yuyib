//! Animated native preview of the Velina fixture.

//
//
// `animated_girl_preview` reuses the same complete skeletal rendering path.
// WASD/Space/Ctrl/Shift and mouse control the camera; Esc exits.
//
// The high-level TexturedSkeletalSceneRenderer3d connects scene nodes,
// GPU-resident base-colour images, mesh uploads and sampled joint palettes.
// Opaque, masked and blended base-colour textures are supported. Normal-map
// PBR skinning and morph normal/tangent deformation remain separate work;
// position morph animation is supported by this preview renderer.

use std::{cell::RefCell, error::Error, path::Path, rc::Rc};

use yuyib::{
    app::{Application, RenderLoop},
    assets::Assets,
    gltf::{
        AnimationClipIndex, AnimationPlayer, ImportOptions, import_scene_path_with_options,
        sample_bind_pose,
    },
    input::{FreeCameraConfig3d, FreeCameraController3d},
    model_assets::ModelTextureLoader,
    platform::WindowConfig,
    render::ClearColor,
    render_3d::{SkeletalTextureResources, TexturedSkeletalSceneRenderer3d},
    render_texture::TextureCache,
    two_d::Texture,
};

/// Runs the shared native skeletal preview for one bundled character fixture.
///
/// # Errors
///
/// Returns import, camera, texture, GPU setup or application lifecycle errors.
///
/// # Panics
///
/// Panics only if already-validated frame timing, animation indices or
/// renderer setup invariants are violated after initialization.
#[allow(clippy::too_many_lines)] // The complete native example is intentionally readable in one place.
pub fn run(girl_preview: bool) -> Result<(), Box<dyn Error>> {
    let (fixture_name, title, label, options) = if girl_preview {
        (
            "sci-fi_girl_v.02_walkcycle_test.glb",
            "Yuyib — sci-fi girl walk-cycle preview",
            "Sci-fi girl",
            ImportOptions::skeletal_preview(),
        )
    } else {
        (
            "velina_zzz.glb",
            "Yuyib — Velina skeletal preview",
            "Velina",
            ImportOptions::skeletal(),
        )
    };
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../for_tests")
        .join(fixture_name);
    let asset = Rc::new(import_scene_path_with_options(fixture_path, options)?);
    if !asset.report().is_complete() {
        eprintln!(
            "{label} preview omissions: primitives={}",
            asset.report().skipped_primitive_count(),
        );
    }
    if asset.scene.skins().is_empty() {
        return Err(format!("{fixture_name} imported without a skeleton").into());
    }
    let texture_loader = Rc::new(ModelTextureLoader::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../for_tests"
    ))?);

    let camera = Rc::new(RefCell::new(FreeCameraController3d::looking_at(
        FreeCameraConfig3d {
            move_speed: 3.5,
            near: 0.02,
            far: 100.0,
            ..FreeCameraConfig3d::default()
        },
        [0.0, 1.25, 3.2],
        [0.0, 1.05, 0.0],
    )?));
    let initial_cursor = camera.borrow().initial_cursor_control();
    let player = Rc::new(RefCell::new(
        (!asset.scene.animations().is_empty())
            .then(|| AnimationPlayer::new(AnimationClipIndex::new(0))),
    ));
    let gpu_renderer = Rc::new(RefCell::new(None::<TexturedSkeletalSceneRenderer3d>));
    let texture_state = Rc::new(RefCell::new(
        None::<(
            Assets<Texture>,
            TextureCache,
            yuyib::model_assets::ModelTextureBindings,
        )>,
    ));

    let window_camera = Rc::clone(&camera);
    let device_camera = Rc::clone(&camera);
    let update_camera = Rc::clone(&camera);
    let render_camera = Rc::clone(&camera);
    let update_player = Rc::clone(&player);
    let render_player = Rc::clone(&player);
    let render_asset = Rc::clone(&asset);
    let render_gpu = Rc::clone(&gpu_renderer);
    let render_texture_loader = Rc::clone(&texture_loader);
    let render_texture_state = Rc::clone(&texture_state);

    Application::new()
        .window(WindowConfig {
            title: title.to_owned(),
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.018, 0.024, 0.042, 1.0))
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
            let delta_seconds = context.frame().delta.as_secs_f32();
            update_camera
                .borrow_mut()
                .step(delta_seconds)
                .expect("camera accepts the application's finite frame delta");
            if let Some(player) = update_player.borrow_mut().as_mut() {
                player
                    .advance(&asset.scene, delta_seconds)
                    .expect("checked clip remains valid");
            }
        })
        .on_render(move |frame| {
            let mut renderer_slot = render_gpu.borrow_mut();
            if renderer_slot.is_none() {
                let mut texture_slot = render_texture_state.borrow_mut();
                if texture_slot.is_none() {
                    let mut textures = Assets::new();
                    let mut gpu_textures = TextureCache::new();
                    match render_texture_loader.load_for_frame(
                        frame,
                        &render_asset.model,
                        &mut textures,
                        &mut gpu_textures,
                    ) {
                        Ok(bindings) => *texture_slot = Some((textures, gpu_textures, bindings)),
                        Err(error) => {
                            eprintln!("{label} texture setup failed: {error}");
                            return;
                        }
                    }
                }
                match TexturedSkeletalSceneRenderer3d::new_for_frame(
                    frame,
                    &render_asset.model,
                    &render_asset.scene,
                ) {
                    Ok(renderer) => {
                        if renderer.factor_only_primitive_count() != 0 {
                            eprintln!(
                                "{label}: {} primitive(s) use their base-colour factor because no usable UV0 texture is present",
                                renderer.factor_only_primitive_count()
                            );
                        }
                        *renderer_slot = Some(renderer);
                    }
                    Err(error) => {
                        eprintln!("{label} GPU setup failed: {error}");
                        return;
                    }
                }
            }
            let renderer = renderer_slot
                .as_ref()
                .expect("the slot was populated by the checked setup branch");
            let pose = match render_player.borrow().as_ref().map_or_else(
                || sample_bind_pose(&render_asset.scene),
                |player| player.snapshot(&render_asset.scene),
            ) {
                Ok(pose) => pose,
                Err(error) => {
                    eprintln!("{label} pose sampling failed: {error}");
                    return;
                }
            };
            let texture_slot = render_texture_state.borrow();
            let (_, gpu_textures, bindings) = texture_slot
                .as_ref()
                .expect("texture state was populated with the renderer");
            if let Err(error) = renderer.draw(
                frame,
                render_camera.borrow().camera(),
                &render_asset.scene,
                &pose,
                SkeletalTextureResources {
                    bindings,
                    textures: gpu_textures,
                },
            ) {
                eprintln!("{label} skeletal render failed: {error}");
            }
        })
        .run()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    run(false)
}
