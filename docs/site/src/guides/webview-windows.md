# WebView: Windows Phase 1

> **Статус:** Experimental, Windows only  
> **Facade feature:** `yuyib = { features = ["webview"] }`  
> **Module:** `yuyib::webview` (or direct `yuyib-webview`)

The first runtime backend is a real Wry/WebView2 native child overlay attached
to a `yuyib::platform::Window`. It is not a WGPU texture and does not require
JSX: use ordinary HTML, CSS and browser JavaScript.

For the common full-window path, enable the facade feature and pass a
`WebViewBuilder` to `Application::webview(...)`. It remains a native child
overlay above `on_render`/native UI; it is not a composited WGPU texture.

`WebViewBuilder` serves developer-owned inline HTML by default, or accepts one
explicitly parsed `ControlledUrl` HTTPS page. Bounds/visibility are controlled
by `WebViewHost`; downloads and all navigation after the first controlled URL
load are blocked. The host is UI-thread-affine and must be dropped before its
parent window. Windows must have Microsoft Edge WebView2 Runtime installed.

## Security boundary

There is no generic JavaScript-to-Rust dispatch, filesystem resolver, remote
allow-list, download support or arbitrary browser capability. Local pages use
the explicit typed bridge described below; it does not grant filesystem,
process, host-object or arbitrary-script power.
`evaluate_developer_script` is only for static developer-owned code—never
construct script by interpolating external data. Inline HTML is capped at 2
MiB and has `null` origin.

Phase 2 provides the pure security foundation for the bridge:
`AssetPath`/in-memory `AssetBundle` reject filesystem-shaped paths by design;
`MimePolicy` is an allow-list and WebAssembly requires explicit opt-in;
`LocalCsp` starts restrictive. `BridgeEnvelope` validates a version, current
`PageSessionId`, non-zero request ID, endpoint name and bounded canonical JSON
payload before any future Wry IPC callback can dispatch it. These types do not
yet expose a browser endpoint or access to host resources.

Phase 3 connects that foundation to a real Wry custom protocol. `LocalPage`
serves an in-memory HTML entry through `app://` (Windows Wry maps it to
`https://app.localhost/...`); only empty-body `GET`/`HEAD` requests to that
fixed origin and declared logical assets are accepted. Responses have CSP,
allow-listed MIME and `X-Content-Type-Options: nosniff`. Navigation stays
locked to the entry page; downloads and popups remain blocked.

`BridgeRouter`/`TypedEndpoint` accept page-to-host requests only after origin,
session, envelope and payload validation. The injected page bootstrap provides
`window.yuyib.post(...)`; it does not expose filesystem, process, host objects
or a generic JavaScript execution channel. `PageEvent`/
`WebViewHost::emit_event` sends a bounded validated event only to the current
local-page session. The fixed bootstrap dispatches `CustomEvent("yuyib:event")`;
the JSON envelope is serialized as a JavaScript string literal before
`JSON.parse`, never concatenated as executable payload.

## Limits & Caveats

WebView is a native overlay above the renderer: it cannot yet be sampled on a
3D mesh or interleaved with WGPU draw order. Input focus and accessibility
remain separate phases. The high-level `Application::webview` path now has a
bounded host-to-page event queue; it is not a general browser or renderer
integration. Full lifecycle and security rationale:
[WebView architecture RFC](../concepts/webview-architecture.md).

## High-level outbound PageEvent queue

For a full-client `Application`, wrap the prepared `WebViewBuilder` before
attaching it:

```rust
let (webview, page) = ApplicationWebView::new(builder).with_event_queue(64)?;
let limits = BridgeLimits::default();
let session = page.page_session()?; // only after native child + local bridge exist
let event = PageEvent::from_typed(
    limits.protocol_version(),
    session,
    EndpointName::parse("ui.status")?,
    "saved",
    limits,
)?;
page.enqueue(event)?;
```

The `page_session`/`enqueue` calls belong in an Application UI-thread callback,
normally `on_frame`; the snippet isolates their exact API and deliberately
omits lifecycle handling. Create the handle once with
`ApplicationWebView::with_event_queue`, then pass `webview` to
`Application::webview`. Before the local typed bridge is live it returns
`NotReady`, without that bridge `NoLocalBridge`, after teardown `Closed`, and
an old session returns `StaleSession`. The fixed-capacity queue is FIFO; it
never blocks or drops an old item, returning `Full { capacity }` instead.

Application flushes queued events after `on_frame` and before that redraw's
GPU work. Successful `enqueue` wakes the Winit event loop and schedules a
redraw even with `RenderLoop::OnDemand`; an event queued from `on_render`
therefore waits for that automatically scheduled next redraw. The handle uses
`Rc` and is `!Send + !Sync`: workers must not call it directly. Wakeups are
coalesced while FIFO has pending items, so a burst creates at most one
outstanding Winit wake before drain.
See the complete runnable lifecycle pattern in
[Native Application](application.md#webview-bounded-host-to-page-events).

Full API: [WebView backend](../api/yuyib_webview/index.html).

## Manual smoke example

On a Windows desktop with WebView2 Runtime installed, run:

```powershell
cargo run -p yuyib-webview --example local_page_smoke --target x86_64-pc-windows-msvc
```

The example owns the native window and `WebViewHost` on the UI thread, serves
in-memory `index.html`/`app.css`/`app.js` (plain browser files), and accepts a
harmless typed `demo.ping` request. It is a manual smoke test, intentionally
outside the default test suite because it opens a desktop child control.

The high-level facade example uses the normal Application lifecycle:

```powershell
cargo run -p yuyib --example application_webview --features webview --target x86_64-pc-windows-msvc
```

На Windows Wry показывает local origin как `https://app.localhost`, но перед
вызовом custom-protocol handler восстанавливает внутренний URL
`app://localhost`. Resolver принимает оба представления только для host
`localhost`; другие scheme/host по-прежнему получают `403`. Это важно при
собственном protocol adapter: проверка только browser-visible origin ошибочно
запретит entry page самому себе.

It demonstrates `Application::webview`, a local page and a typed inbound
endpoint. `ApplicationWebView::with_event_queue` additionally provides the
supported high-level outbound `PageEvent` path without exposing `WebViewHost`.

`webview` — необязательная возможность facade. Поэтому команда без
`--features webview` намеренно завершается сообщением Cargo о требуемой
возможности: по умолчанию проект не тянет WebView2 и связанные зависимости
в нативные приложения и игры, которым WebView не нужен.
