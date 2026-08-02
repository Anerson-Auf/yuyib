//! Manual Windows example for a high-level local `WebView` application.
//!
//! Run only on a Windows desktop with the Microsoft Edge `WebView2` Runtime:
//! `cargo run -p yuyib --example application_webview --features webview --target x86_64-pc-windows-msvc`
//!
//! The page contains ordinary inline HTML, CSS, and JavaScript. It needs no
//! JSX, Node runtime, filesystem asset resolver, remote navigation, or raw
//! Wry API.
//!
//! The example uses both bridge directions without exposing `WebViewHost`:
//! a typed page request enters Rust and a bounded `PageEvent` is queued back to
//! the current local-page session.

use std::{cell::RefCell, error::Error, rc::Rc};

use serde::{Deserialize, Serialize};
use yuyib::{
    app::{Application, ApplicationWebView, ApplicationWebViewHandle, RenderLoop},
    platform::WindowConfig,
    webview::{
        AssetBundle, AssetLimits, AssetPath, BridgeLimits, BridgeRouter, EndpointName, LocalCsp,
        LocalPage, MimePolicy, PageEvent, PageSessionId, TypedEndpoint, WebViewBuilder,
    },
};

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Yuyib Application WebView</title>
    <link rel="stylesheet" href="./app.css">
    <script type="module" src="./app.js"></script>
  </head>
  <body>
    <main>
      <p class="eyebrow">Native Application + local WebView</p>
      <h1>Plain web UI, typed Rust endpoint</h1>
      <p>Nothing here is fetched from the network or filesystem.</p>
      <button id="send" type="button">Send typed message to Rust</button>
      <output id="status" aria-live="polite"></output>
    </main>
  </body>
</html>"#;

const APP_CSS: &str = r"body {
  margin: 0;
  min-height: 100vh;
  display: grid;
  place-items: center;
  color: #e8eef8;
  background: radial-gradient(circle at top, #253251, #11151e 60%);
  font: 16px/1.5 system-ui, sans-serif;
}
main {
  box-sizing: border-box;
  width: min(38rem, 90vw);
  padding: 2.25rem;
  border: 1px solid #4f6391;
  border-radius: 1rem;
  background: #1a2233e8;
  box-shadow: 0 1.25rem 4rem #0008;
}
.eyebrow {
  margin: 0;
  color: #9db9ff;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}
h1 {
  margin: 0.45rem 0;
}
button {
  margin-top: 0.75rem;
  padding: 0.7rem 1rem;
  border: 0;
  border-radius: 0.5rem;
  color: #07101f;
  background: #9db9ff;
  cursor: pointer;
  font: inherit;
  font-weight: 700;
}
output {
  display: block;
  min-height: 1.5em;
  margin-top: 0.9rem;
  color: #c5f6d0;
}";

const APP_JS: &str = r##"const button = document.querySelector("#send");
const status = document.querySelector("#status");
let nextId = 1;

button.addEventListener("click", () => {
  window.yuyib.post({
    version: 1,
    id: nextId++,
    endpoint: "demo.hello",
    payload: { text: "Hello from plain JavaScript" }
  });
  status.textContent = "Waiting for the bounded Rust reply…";
});

window.addEventListener("yuyib:event", ({ detail }) => {
  if (detail.event === "demo.ack") {
    status.textContent = detail.payload.text;
  }
});"##;

#[derive(Deserialize)]
struct HelloMessage {
    text: String,
}

#[derive(Serialize)]
struct Acknowledgement {
    text: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    let entry = AssetPath::parse("index.html")?;
    let mut assets = AssetBundle::new(MimePolicy::strict(), AssetLimits::default());
    assets.insert(entry.clone(), INDEX_HTML)?;
    assets.insert(AssetPath::parse("app.css")?, APP_CSS)?;
    assets.insert(AssetPath::parse("app.js")?, APP_JS)?;

    let page = LocalPage::new(entry, assets, LocalCsp::strict())?;
    let session = PageSessionId::parse("f8dfb84b8b604b5c8644efacaa699bad")?;
    let bridge_limits = BridgeLimits::default();
    let outbound = Rc::new(RefCell::new(None::<ApplicationWebViewHandle>));
    let endpoint_outbound = Rc::clone(&outbound);
    let mut bridge = BridgeRouter::new(session, bridge_limits);
    bridge.register(TypedEndpoint::new(
        EndpointName::parse("demo.hello")?,
        move |message: HelloMessage| {
            let Some(handle) = endpoint_outbound.borrow().clone() else {
                eprintln!("outbound WebView handle is not installed");
                return;
            };
            let event = PageEvent::from_typed(
                bridge_limits.protocol_version(),
                session,
                EndpointName::parse("demo.ack").expect("static event name is valid"),
                Acknowledgement {
                    text: format!("Rust accepted: {}", message.text),
                },
                bridge_limits,
            );
            match event {
                Ok(event) => {
                    if let Err(error) = handle.enqueue(event) {
                        eprintln!("could not queue demo.ack: {error}");
                    }
                }
                Err(error) => eprintln!("could not build demo.ack: {error}"),
            }
        },
    ))?;

    let builder = WebViewBuilder::new()
        .with_local_page(page)
        .with_bridge_router(bridge);
    let (webview, handle) = ApplicationWebView::new(builder).with_event_queue(8)?;
    *outbound.borrow_mut() = Some(handle);

    Application::new()
        .window(WindowConfig {
            title: "Yuyib Application WebView".to_owned(),
            width: 960,
            height: 640,
            ..Default::default()
        })
        .render_loop(RenderLoop::OnDemand)
        .webview(webview)
        .run()?;

    Ok(())
}
