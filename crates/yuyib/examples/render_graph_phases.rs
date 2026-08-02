//! Declared render phases and dependencies over one application frame.

use std::error::Error;

use yuyib::{
    app::{Application, RenderLoop},
    platform::WindowConfig,
    render::{
        GraphPassDescriptor, RenderGraph, RenderPhase, RenderResourceId,
        wgpu::{Color, LoadOp},
    },
};

fn main() -> Result<(), Box<dyn Error>> {
    let surface = RenderResourceId::surface_color();
    let mut graph = RenderGraph::new();
    let world = graph.add_infallible_pass(
        GraphPassDescriptor::new("example.world", RenderPhase::Opaque3d).writes(surface.clone()),
        |frame| {
            frame.with_surface_pass(
                LoadOp::Clear(Color {
                    r: 0.025,
                    g: 0.08,
                    b: 0.14,
                    a: 1.0,
                }),
                |_| {},
            );
        },
    )?;
    graph.add_infallible_pass(
        GraphPassDescriptor::new("example.post-process", RenderPhase::PostProcess)
            .after(world)
            .reads(surface.clone())
            .writes(surface),
        |frame| {
            // A real post-process pipeline would sample an intermediate scene
            // texture here. This pass still proves ordering and scoped access.
            frame.with_surface_pass(LoadOp::Load, |_| {});
        },
    )?;

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — declared render graph".to_owned(),
            ..Default::default()
        })
        .render_loop(RenderLoop::Continuous)
        .render_graph(graph)
        .run()?;
    Ok(())
}
