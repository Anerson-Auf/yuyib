//! Native Yuyib Editor host.

mod app;
mod bridge;
mod editor_gizmo;
mod gltf_preview;
mod lsp_ra;
mod scene_authoring;
mod scene_interaction;
mod viewport_gizmo;
mod viewport_picking;

use std::{env, error::Error, path::PathBuf};

use app::EditorApp;
use yuyib_platform::winit::event_loop::EventLoop;

include!(concat!(env!("OUT_DIR"), "/editor_assets.rs"));

fn main() -> Result<(), Box<dyn Error>> {
    // Never treat the process cwd (often the yuyib monorepo) as an open project.
    // Only an explicit CLI path with a valid project.yuyib opens immediately;
    // otherwise the host starts empty and the UI stays on the project launcher.
    let editor = match env::args_os().nth(1) {
        Some(path) => EditorApp::from_project_path(PathBuf::from(path))?,
        None => EditorApp::empty()?,
    };
    let mut editor = editor;
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut editor)?;
    Ok(())
}
