//! Interactive specular IBL probe lab (M2.2).
//!
//! Metallic cubes at three roughnesses under a synthetic asymmetric
//! prefiltered environment. Orbit with mouse, move with WASD, Esc exits.
//!
//! ```text
//! cargo run -p yuyib --example specular_ibl_window
//! ```
//!
//! This is the visual check for factor-only split-sum IBL. The street-city
//! playable path also enables textured specular IBL via the same environment.

use std::{cell::RefCell, error::Error, rc::Rc};

use yuyib::{
    app::{Application, RenderLoop},
    input::{FreeCameraConfig3d, FreeCameraController3d},
    model::MeshPrimitive,
    platform::{CursorControl, WindowConfig},
    render::ClearColor,
    render_3d::{
        DepthLoad, DiffuseIrradianceSh3d, GpuSpecularIbl3d, LambertLighting3d, MeshTransform3d,
        PbrLighting3d, PbrMaterial3d, PbrMeshRenderer3d, PreparedSpecularIbl3d,
    },
};

fn main() -> Result<(), Box<dyn Error>> {
    let prepared = PreparedSpecularIbl3d::synthetic_asymmetric()?;
    let cube = MeshPrimitive::cube(0.4)?;
    let direct = LambertLighting3d::artistic(
        [-0.25, -1.0, -0.3],
        [1.0, 0.98, 0.94],
        0.45,
        [0.06, 0.07, 0.09],
    )?;
    let diffuse = DiffuseIrradianceSh3d::constant([0.1, 0.11, 0.14])?;
    let lighting = PbrLighting3d::new(direct, diffuse).with_specular_ibl_strength(1.0);
    let probes = [
        (-1.3_f32, 0.06_f32, "smooth"),
        (0.0, 0.35, "mid"),
        (1.3, 0.9, "rough"),
    ];

    let camera = Rc::new(RefCell::new(FreeCameraController3d::looking_at(
        FreeCameraConfig3d {
            move_speed: 3.5,
            ..FreeCameraConfig3d::default()
        },
        [0.0, 0.9, 4.2],
        [0.0, 0.0, 0.0],
    )?));
    let window_camera = Rc::clone(&camera);
    let device_camera = Rc::clone(&camera);
    let update_camera = Rc::clone(&camera);
    let render_camera = Rc::clone(&camera);
    let cursor_ready = Rc::new(RefCell::new(false));
    let update_cursor = Rc::clone(&cursor_ready);

    println!(
        "specular_ibl_window: metallic probes under synthetic IBL — look for coloured \
         environment reflections that blur with roughness (WASD + mouse, Esc)."
    );

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — specular IBL probes".to_owned(),
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.015, 0.02, 0.035, 1.0))
        .render_loop(RenderLoop::Continuous)
        .cursor_control(CursorControl::Released)
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
            if !*update_cursor.borrow() {
                context.set_cursor_control(update_camera.borrow().initial_cursor_control());
                *update_cursor.borrow_mut() = true;
            }
            if let Err(error) = update_camera
                .borrow_mut()
                .step(context.frame().delta.as_secs_f32())
            {
                eprintln!("specular_ibl_window camera: {error}");
            }
        })
        .on_render(move |frame| {
            let meshes = PbrMeshRenderer3d::new_for_frame(frame);
            let ibl = GpuSpecularIbl3d::upload_for_frame(frame, &prepared);
            let gpu_cube = match meshes.upload_mesh_for_frame(frame, &cube) {
                Ok(mesh) => mesh,
                Err(error) => {
                    eprintln!("specular_ibl_window upload: {error}");
                    return;
                }
            };
            let camera = render_camera.borrow().camera();
            for (index, (x, roughness, _label)) in probes.iter().copied().enumerate() {
                let Ok(model) =
                    MeshTransform3d::new([x, 0.0, 0.0], [0.0, 0.4, 0.0], [1.0, 1.0, 1.0]).matrix()
                else {
                    continue;
                };
                let Ok(material) = PbrMaterial3d::new([0.92, 0.92, 0.95, 1.0], 1.0, roughness) else {
                    continue;
                };
                let depth = if index == 0 {
                    DepthLoad::Clear
                } else {
                    DepthLoad::Load
                };
                if let Err(error) = meshes.draw_with_specular_ibl(
                    frame,
                    camera,
                    &gpu_cube,
                    model,
                    material,
                    lighting,
                    depth,
                    false,
                    Some(&ibl),
                    None,
                ) {
                    eprintln!("specular_ibl_window draw: {error}");
                    return;
                }
            }
        })
        .run()?;
    Ok(())
}
