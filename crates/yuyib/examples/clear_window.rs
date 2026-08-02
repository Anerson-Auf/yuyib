//! First high-level Yuyib application: a native window and WGPU surface.

use yuyib::{
    app::{Application, RenderLoop},
    platform::WindowConfig,
};

fn main() -> Result<(), yuyib::app::ApplicationError> {
    Application::new()
        .window(WindowConfig {
            title: "Yuyib — first GPU surface".to_owned(),
            ..Default::default()
        })
        .render_loop(RenderLoop::Continuous)
        .on_frame(|frame| {
            if frame.frame().index == 600 {
                frame.request_exit();
            }
        })
        .run()
}
