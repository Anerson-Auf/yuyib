# WebView architecture (Windows RFC)

> Status: current Windows Experimental slice plus RFC for deferred WebView
> phases. `yuyib-webview` and feature-gated `Application::webview` are public
> experimental APIs; this page does not make them stable or cross-platform.

Yuyib is **native UI first**. A WebView is an optional, first-class window
surface for applications that benefit from ordinary HTML, CSS, and JavaScript:
documentation, account flows, editors, dashboards, rich text, and rapidly
iterated UI. It must not become a required language, build system, or
application architecture.

On Windows the initial backend is Microsoft Edge WebView2, accessed through
[Wry 0.55.1](https://docs.rs/wry/0.55.1/wry/). Wry is a direct Rust wrapper
maintained in the Tauri ecosystem; using it does **not** make an application a
Tauri application and does not impose Tauri's command/build conventions.

## Decision and compatibility

| Item | Initial decision | Why |
| --- | --- | --- |
| Host crate | `wry = "0.55.1"` | Actively maintained native WebView abstraction with Windows WebView2 support. |
| Window crate | existing `winit = "0.30.12"` | Wry's `WebViewBuilder::build` accepts a window implementing `HasWindowHandle`; Winit 0.30 windows provide that handle. |
| Native engine | Microsoft Edge WebView2 Runtime | The Windows backend used by Wry. |
| Handle ABI | `raw-window-handle 0.6` | The compatibility boundary used by the evaluated Wry/Winit releases. |
| Public policy | WebView feature-gated, Windows-only initially | Keeps native-only applications free of WebView2 deployment and attack surface. |

This compatibility is based on the published APIs for
[Wry's builder](https://docs.rs/wry/0.55.1/wry/struct.WebViewBuilder.html) and
[Winit's Window](https://docs.rs/winit/0.30.12/winit/window/struct.Window.html).
Phase 1 includes a Windows compile-and-smoke test for the committed dependency.
The only assumption here is that Wry 0.55.1 and the workspace's
exact Winit patch release remain compatible through their `HasWindowHandle`
contract; it is not a promise about future Wry or Winit upgrades.

### Rejected initial alternatives

1. **Direct Wry — selected.** It supplies the native child WebView and custom
   protocol needed for a small, explicit framework layer.
2. **`webview2-com` — reserve for a later internal backend.** It is appropriate
   when Yuyib needs Windows-only CompositionController, visual hosting, or an
   unwrapped WebView2 API. Starting there would leak COM lifetime details and
   unsafe implementation complexity into a framework whose public policy is
   `unsafe_code = "forbid"`.
3. **Tauri application API — not selected.** It solves packaging and app
   conventions, but that is a different product boundary. Yuyib needs a small
   embeddable library, not a second application framework.

Yuyib's public WebView API will not require unsafe code. A dependency may use
unsafe internally; that is contained behind the optional backend crate and
reviewed like any other native dependency.

## What a WebView is—and is not—in the renderer

The first implementation is a **native child/overlay surface** inside a
Yuyib/Winit window. It is not a `wgpu` texture and is not rendered by
`RenderFrame`. Its bounds, visibility, focus, and z-order are coordinated by
the window runtime.

Граница:

| Supported in first backend | Explicitly deferred |
| --- | --- |
| A web panel or full-window page above native content | Drawing a live browser page on a 3D mesh/HUD |
| Resizing/visibility/focus from the UI thread | Sampling WebView output as a GPU texture |
| Native app ↔ page messages | Arbitrary mixing of browser and renderer draw order |

WebView2 composition hosting may make texture-like integration possible later,
but it needs a separate Windows graphics design, synchronization model, and
security review. The initial API must not pretend that a browser is a normal
renderer texture.

## Ownership, threading, and lifecycle

Winit and the WebView backend are UI-thread-affine. The runtime owns each
attachment as one UI-thread-only unit:

```text
Application event loop / UI thread
  WindowRecord
    Arc<winit::window::Window>
    WebViewHost
      backend WebView
      WebView state + page session
      bounded inbound/outbound queues
```

The sequence is:

1. Create the native window on the application event loop.
2. Attach a WebView only after the window exists.
3. Keep the window handle and backend WebView in the same UI-thread-owned
   `WebViewHost`. Neither is `Send` or shared directly with workers.
4. Worker/game/network tasks exchange owned messages through bounded channels.
   The UI thread alone pumps them into or out of the browser.
5. When a window closes, stop forwarding callbacks, invalidate the current page
   session, drop the WebView, then release the window reference.

Wry documents that the WebView is built from a native window handle and that,
on Windows, it automatically follows the parent window's size. Yuyib still
tracks desired bounds itself so that hiding, layout changes, and future
non-child hosting have one consistent API. See
[Wry WebViewBuilder](https://docs.rs/wry/0.55.1/wry/struct.WebViewBuilder.html).

No callback may retain a stale page or window after navigation/close. Every
bridge message is associated with a generated `PageSessionId`; messages from a
previous session are discarded. This is required because browser messages and
navigation can race.

## Authoring model: standard web files, no JSX

An application may contain ordinary files:

```text
ui/
  index.html
  app.css
  app.js
  icons/
    save.svg
```

```html
<!-- ui/index.html -->
<link rel="stylesheet" href="./app.css">
<main id="app"></main>
<script type="module" src="./app.js"></script>
```

```js
// ui/app.js — plain ES module, no JSX or Node runtime required.
window.yuyib.post({ version: 1, id: 41, endpoint: "document.save", payload: {} });
```

HTML, CSS, and browser-standard ES modules are the baseline. React, Vue, Svelte
or a JSX/TypeScript pipeline may be chosen by an application, but they are
external build choices, never a Yuyib requirement. Tailwind is likewise
supported as *compiled CSS emitted into the asset bundle*; Yuyib does not run
Tailwind at runtime.

## Local assets and navigation policy

The default page is served by an internal custom protocol—conceptually
`app://…`—backed by an `AssetResolver`. The actual URL spelling is backend
dependent, so application code must use `WebOrigin::Local` rather than rely on
a hard-coded host name. Wry's
[custom protocol API](https://docs.rs/wry/0.55.1/wry/struct.WebViewBuilder.html)
is the intended backend mechanism.

Rules for the local resolver:

- Asset IDs are logical, slash-separated paths under one registered bundle.
  Absolute paths, decoded `..` traversal, drive prefixes, and symlink escape
  are rejected.
- `file://` navigation and arbitrary disk reads are disabled. A custom protocol
  must never become a general file server.
- Responses use an explicit MIME map, including UTF-8 text types, JavaScript,
  CSS, images, fonts, and WASM only when opted in.
- A restrictive default Content Security Policy is emitted for local pages:
  `default-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none';
  connect-src 'self'`. Individual relaxations are explicit configuration, not
  implicit behavior.
- Remote navigation is denied by default. It requires a `TrustedOrigin`
  allow-list (scheme, host, optional port/path policy) and has no access to
  privileged bridge endpoints unless separately granted.

This policy follows WebView2's recommendation to treat web content as
untrusted and constrain navigation and host integration; see Microsoft's
[WebView2 security guidance](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/security).

## Typed bridge and security boundary

The bridge is a message protocol, not a mechanism for JavaScript to call
arbitrary Rust functions. WebView2 exposes page-to-host messaging through
`window.chrome.webview.postMessage`; Yuyib provides a small wrapper
`window.yuyib.post` and a symmetric typed host-to-page event API. Microsoft recommends
JSON messages and validation rather than string/script injection:
[WebView2 control overview](https://learn.microsoft.com/en-us/windows/apps/develop/ui/controls/webview2)
and [security guidance](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/security).

Conceptual envelope:

```json
{
  "version": 1,
  "session": "page-session-id",
  "id": 41,
  "kind": "document.save",
  "payload": {}
}
```

Every endpoint is registered in Rust with:

- a stable name and protocol version;
- a request and response/event schema;
- maximum message and payload byte sizes;
- its allowed origins and explicit capability set;
- a handler that returns a typed result or a typed, non-sensitive error.

The Rust implementation uses `serde` for schema data, but the public web
contract stays JSON objects, so the page has no Rust-specific language/tooling
requirement. The bridge parser and endpoint router are pure, testable code
before they touch Wry.

Security invariants:

- Validate origin, page session, version, endpoint name, message size, and
  schema before dispatch.
- Unknown endpoints and malformed payloads fail closed and produce structured,
  rate-limited diagnostics—not panics or raw internal errors.
- Browser content gets no filesystem, process, native window, network, or
  renderer access by default. Each privileged operation is represented by a
  narrowly scoped capability endpoint.
- Never construct executable JavaScript by interpolating untrusted strings.
  Host-to-page communication is serialized data, not `eval`.
- Host objects, devtools, context menus, downloads, popups, and remote
  navigation are opt-in and separately modeled. Development defaults must not
  silently become release defaults.
- Correlation IDs permit replies, but are not authorization. The current
  `PageSessionId` prevents a late message from an earlier navigation being
  accepted.

## Current high-level outbound events

The current `yuyib-app` facade provides one deliberate host-to-page path:
`ApplicationWebView::with_event_queue(capacity)`. It returns an
`ApplicationWebViewHandle` that accepts only prevalidated, session-bound
`PageEvent` values. The handle is UI-thread-only (`!Send + !Sync`), has no
`WebViewHost`/Wry escape hatch, and accepts events only for the current local
typed-bridge session.

The queue is bounded FIFO. It rejects—not drops or blocks—on zero configured
capacity, full queue, stale session, absence of a local bridge, pre-creation
use, or close. A successful enqueue wakes Winit and schedules the next redraw
even under `RenderLoop::OnDemand`. At redraw, Application drains it after
`on_frame` and before GPU rendering; an event from `on_render` therefore uses
that automatically scheduled following redraw. Pending bursts coalesce to at
most one outstanding Winit wake before the FIFO is drained.
See the exact setup and error/lifecycle policy in
[Native Application](../guides/application.md#webview-bounded-host-to-page-events).

## Current and proposed API layers

The high-level queue below is current public API. The lower-level names and
extensions remain an RFC proposal.

### High-level application API

The common path should be one attachment declaration, not a multi-stage
framework ritual:

```rust
let (webview, page) = ApplicationWebView::new(builder).with_event_queue(64)?;
let app = Application::new()
    .window(WindowConfig::default())
    .webview(webview);
```

Use `page.page_session()` and `PageEvent::from_typed(...)` from an application
UI-thread callback to enqueue an event. The application keeps its existing
Rust logic; HTML is merely an optional presentation surface.

### Lower-level integration API

Applications that need custom hosting can use explicit pieces without dropping
to raw Win32/COM:

```rust
let resolver = AssetResolver::embedded(assets)
    .with_csp(LocalCsp::strict());

let bridge = BridgeRouter::new()
    .register(BridgeEndpoint::typed::<SaveRequest, SaveReply>(
        "document.save",
        Capability::DocumentSave,
        save_document,
    ));

let host = WebViewHostBuilder::new(window)
    .source(WebPage::local("index.html"))
    .assets(resolver)
    .bridge(bridge)
    .permissions(WebPermissions::local_only())
    .build_on_ui_thread()?;
```

This layer exposes lifecycle, bounds, visibility, navigation policy, resolver,
and bridge configuration. It deliberately does not expose raw Wry/COM handles
as the default escape hatch. A narrowly documented backend extension trait can
be considered only after a concrete advanced use case exists.

## Delivery state and deferred phases

| Phase | Scope | Explicit non-goals |
| --- | --- | --- |
| 0 | Contract, threat model, compatibility decision | Not a stability or cross-platform claim |
| 1 — delivered | Optional `yuyib-webview`; local page; child hosting; resize/close lifecycle | GPU-texture WebViews, generic JS execution |
| 2 — delivered | In-memory local assets, strict navigation, typed bounded bridge and current-session page events | Host filesystem/network power by default |
| 3 — partial | `Application::webview` plus bounded UI-thread outbound queue | Dev-only tooling, installer/runtime diagnostics, cross-platform claim |
| 4 | Explicit trusted remote origins and additional capabilities after security review | Broad allow-all web access |
| 5 | Evaluate WebView2 composition/3D texture integration as a separate renderer RFC | Treating it as a minor extension of Phase 1 |

## Windows deployment assumption

WebView2 Runtime is a deployment prerequisite. The Windows installer must choose
and document either an Evergreen Runtime bootstrapper or a pinned fixed-version
runtime strategy; application startup must report a clear remediation when the
runtime is unavailable. This is an explicit product/installer assumption, not
something the Rust library can safely hide. Microsoft documents runtime
distribution options in
[WebView2 distribution guidance](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution).

## Definition of done for Phase 1–2

- A Windows integration smoke test creates a Winit 0.30.12 window, attaches a
  Wry 0.55.1 WebView, serves a local page, and shuts down cleanly.
- The host compiles with Yuyib's `unsafe_code = "forbid"` policy.
- Unit tests reject traversal and malformed/oversize bridge messages, reject
  disallowed navigation, and discard stale page-session events.
- A sample uses only HTML/CSS/plain JavaScript and requires no JSX build chain.
- Rustdoc and the project API wiki describe every stable option, its defaults,
  capability implications, limits, and Windows/WebView2 prerequisite.
