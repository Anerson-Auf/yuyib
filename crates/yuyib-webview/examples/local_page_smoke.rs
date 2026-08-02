//! Manual Windows smoke example for a local HTML/CSS/JavaScript `WebView` page.
//!
//! Run only on a Windows desktop with the Microsoft Edge `WebView2` Runtime:
//! cargo run -p yuyib-webview --example `local_page_smoke` --target x86_64-pc-windows-msvc
//!
//! The page uses ordinary HTML, CSS, and a plain ES module. It does not use
//! JSX, a Node runtime, a filesystem asset resolver, or application facade.

use std::error::Error;

use serde::Deserialize;
use yuyib_platform::{
    Window, WindowConfig,
    winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        window::WindowId,
    },
};
use yuyib_webview::{
    AssetBundle, AssetLimits, AssetPath, BridgeLimits, BridgeRouter, EndpointName, LocalCsp,
    LocalPage, MimePolicy, PageSessionId, TypedEndpoint, WebViewBounds, WebViewBuilder,
    WebViewHost,
};

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Yuyib local WebView smoke</title>
    <link rel="stylesheet" href="./app.css">
    <script type="module" src="./app.js"></script>
  </head>
  <body>
    <main>
      <h1>Local WebView smoke</h1>
      <p>This page is plain HTML, CSS, and JavaScript.</p>
      <button id="ping" type="button">Send harmless typed message</button>
      <output id="status"></output>
    </main>
  </body>
</html>"#;

const APP_CSS: &str = r"body {
  margin: 0;
  min-height: 100vh;
  display: grid;
  place-items: center;
  background: #171b22;
  color: #edf2f7;
  font: 16px system-ui, sans-serif;
}
main {
  width: min(32rem, 90vw);
  padding: 2rem;
  border-radius: 0.75rem;
  background: #252b36;
}
button {
  padding: 0.65rem 0.9rem;
  border: 0;
  border-radius: 0.4rem;
  background: #5289ff;
  color: white;
  font: inherit;
}";

const APP_JS: &str = r##"const button = document.querySelector("#ping");
const status = document.querySelector("#status");
let nextId = 1;

button.addEventListener("click", () => {
  window.yuyib.post({
    version: 1,
    id: nextId++,
    endpoint: "demo.ping",
    payload: { text: "Hello from plain JavaScript" }
  });
  status.textContent = "Sent a bounded demo.ping message to Rust.";
});"##;

#[derive(Deserialize)]
struct DemoPing {
    text: String,
}

#[derive(Default)]
struct SmokeApp {
    window: Option<Window>,
    webview: Option<WebViewHost>,
}

impl SmokeApp {
    fn create_host(window: &Window) -> Result<WebViewHost, Box<dyn Error>> {
        let entry = AssetPath::parse("index.html")?;
        let mut assets = AssetBundle::new(MimePolicy::strict(), AssetLimits::default());
        assets.insert(entry.clone(), INDEX_HTML)?;
        assets.insert(AssetPath::parse("app.css")?, APP_CSS)?;
        assets.insert(AssetPath::parse("app.js")?, APP_JS)?;

        let page = LocalPage::new(entry, assets, LocalCsp::strict())?;
        let session = PageSessionId::parse("9c42c076c7ca4d0c8e5bf4e61ac22f77")?;
        let mut router = BridgeRouter::new(session, BridgeLimits::default());
        router.register(TypedEndpoint::new(
            EndpointName::parse("demo.ping")?,
            |message: DemoPing| {
                println!(
                    "typed demo.ping accepted on the UI thread: {}",
                    message.text
                );
            },
        ))?;

        WebViewBuilder::new()
            .with_local_page(page)
            .with_bridge_router(router)
            .build(window)
            .map_err(Into::into)
    }

    fn resize_webview(&self) {
        let (Some(window), Some(webview)) = (&self.window, &self.webview) else {
            return;
        };
        let physical = window.physical_size();
        let logical = physical.to_logical::<f64>(window.raw().scale_factor());
        let Ok(bounds) =
            WebViewBounds::new(0.0, 0.0, logical.width.max(1.0), logical.height.max(1.0))
        else {
            return;
        };
        if let Err(error) = webview.set_bounds(bounds) {
            eprintln!("could not resize local WebView: {error}");
        }
    }
}

impl ApplicationHandler for SmokeApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let config = WindowConfig {
            title: "Yuyib local WebView smoke".to_owned(),
            width: 900,
            height: 600,
            ..Default::default()
        };
        let window = match Window::create(event_loop, &config) {
            Ok(window) => window,
            Err(error) => {
                eprintln!("could not create native window: {error}");
                event_loop.exit();
                return;
            }
        };
        let webview = match Self::create_host(&window) {
            Ok(webview) => webview,
            Err(error) => {
                eprintln!("could not create local WebView: {error}");
                event_loop.exit();
                return;
            }
        };

        self.window = Some(window);
        self.webview = Some(webview);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::Resized(_) => self.resize_webview(),
            WindowEvent::CloseRequested => {
                // Drop the child controller before the owning Window. Both
                // values remain on this event-loop/UI thread.
                self.webview.take();
                self.window.take();
                event_loop.exit();
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = SmokeApp::default();
    event_loop.run_app(&mut app)?;
    Ok(())
}
