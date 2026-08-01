//! Phase 0 end-to-end proof: wires discovery + WebSocket transport + all
//! three domains into one real server, with no dependency on the rest of
//! the workspace (`remo-sdk`/`remo-protocol`/`remo-transport` don't exist in
//! this binary's dependency graph at all — that's the whole point of Phase
//! 0 being standalone).
//!
//! `Remo.invoke("ping", {})` mirrors today's `__ping` built-in for a direct
//! before/after comparison.
//!
//! Run: `cargo run -p remo-cdp --example standalone`
//! Then point Chrome at `chrome://inspect` → Configure… → `127.0.0.1:9930`,
//! or paste directly:
//! `devtools://devtools/bundled/inspector.html?ws=127.0.0.1:9930/devtools/page/1`
//!
//! ## Why `main()` isn't `#[tokio::main]`
//!
//! Originally (Phase 0): `remo_objc::run_on_main_sync` dispatched via GCD's real
//! `dispatch_sync_f(dispatch_get_main_queue(), ...)` on *any* Apple target — including bare
//! macOS, not just behind the `uikit` feature — so a bare `#[tokio::main]` binary (tokio's
//! executor owns the main thread; nothing drains libdispatch's main queue) made
//! `DOM.getDocument`/`Page.captureScreenshot` hang forever. Confirmed by actually running it and
//! watching it hang, not a theoretical concern — hence the `dispatch_main()` workaround below.
//!
//! That root cause has since been fixed at the source (Phase 1 of the rewrite): `run_on_main_sync`
//! now only takes the real GCD path behind `all(target_vendor = "apple", feature = "uikit")`,
//! matching every other `remo-objc` module — this crate's own default build (no `--features ios`,
//! the invocation this doc comment tells you to run) takes the direct-call stub instead, which
//! needs no run loop at all. Verified empirically post-fix: `Page.captureScreenshot` against this
//! exact binary now returns immediately (a stub "capture failed", not a hang) with no
//! `dispatch_main()` in the picture.
//!
//! The `std::thread::spawn` + `dispatch_main()` structure below is kept anyway, even though it's
//! no longer load-bearing for this example's own default invocation, because it's still the
//! correct pattern to demonstrate for anyone adapting this into a *real* UIKit-touching standalone
//! binary (`--features ios`, run as a GUI app with an Info.plist) — a real iOS app doesn't need it
//! at all, since `UIApplicationMain`'s own run loop services the main queue for free, but a bare
//! macOS binary that does turn `uikit` on would hit the original Phase 0 hang again without it.

use std::sync::Arc;

use axum::Router;
use remo_cdp::discovery::{self, DiscoveryConfig};
use remo_cdp::dispatcher::Dispatcher;
use remo_cdp::domain_dom::DomDomain;
use remo_cdp::domain_page::PageDomain;
use remo_cdp::domain_remo::{InMemoryCapabilities, RemoDomain};
use remo_cdp::transport;
use serde_json::json;

const PORT: u16 = 9930;

#[allow(unsafe_code)]
fn main() {
    tracing_subscriber::fmt::init();
    std::thread::spawn(run_server);
    // SAFETY: `dispatch_main()` never returns; it parks the calling thread
    // while draining libdispatch's main queue, which is all this call does.
    // No pointers/state cross this FFI boundary.
    unsafe { dispatch_main() };
}

fn run_server() {
    tokio::runtime::Runtime::new()
        .expect("build tokio runtime")
        .block_on(serve());
}

async fn serve() {
    let capabilities = Arc::new(InMemoryCapabilities::new());
    capabilities.register("ping", |_params| Ok(json!({ "pong": true })));

    let build_dispatcher: transport::DispatcherFactory = Arc::new(move || {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(Arc::new(RemoDomain::new(Arc::clone(&capabilities))));
        dispatcher.register(Arc::new(PageDomain::new()));
        dispatcher.register(Arc::new(DomDomain::new()));
        dispatcher
    });

    let app = Router::new()
        .merge(discovery::router(DiscoveryConfig {
            page_title: "Remo standalone".to_string(),
            page_id: "1".to_string(),
        }))
        .merge(transport::router(build_dispatcher))
        .merge(remo_cdp::console::router());

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT))
        .await
        .expect("bind 127.0.0.1:9930 — is another instance already running?");
    println!("remo-cdp standalone listening on 127.0.0.1:{PORT}");
    println!("devtools://devtools/bundled/inspector.html?ws=127.0.0.1:{PORT}/devtools/page/1");
    println!("http://127.0.0.1:{PORT}/console  (zero-install Remo.invoke — no remo-cli needed)");

    axum::serve(listener, app).await.expect("server error");
}

#[link(name = "System", kind = "dylib")]
extern "C" {
    fn dispatch_main() -> !;
}
