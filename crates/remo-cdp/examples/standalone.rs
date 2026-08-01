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
//! `remo_objc::run_on_main_sync` (used by `domain_dom`/`domain_page` to reach
//! UIKit) dispatches via GCD's real `dispatch_sync_f(dispatch_get_main_queue(), ...)`
//! on *any* Apple target — including bare macOS, not just behind the `uikit`
//! feature — so it applies here too. That call blocks until something drains
//! the main dispatch queue. Inside a real iOS app, `UIApplicationMain`'s run
//! loop does that as a side effect of normal event handling. A bare
//! `#[tokio::main]` binary has nothing doing that: tokio's executor owns the
//! main thread instead, and `DOM.getDocument`/`Page.captureScreenshot` hang
//! forever waiting for a main queue nobody is servicing — confirmed by
//! actually running this and watching it hang, not a theoretical concern.
//!
//! The fix for *this standalone binary* (not for the domains — their
//! `run_on_main_sync` usage is exactly correct for a real app process) is to
//! give the real OS main thread a GCD run loop: the server runs on a
//! spawned background thread with its own Tokio runtime, and the actual
//! `main()` thread calls `dispatch_main()`, which parks it while
//! continuously draining the main queue — precisely what a real app's main
//! thread already does for free.

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
        .merge(transport::router(build_dispatcher));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT))
        .await
        .expect("bind 127.0.0.1:9930 — is another instance already running?");
    println!("remo-cdp standalone listening on 127.0.0.1:{PORT}");
    println!("devtools://devtools/bundled/inspector.html?ws=127.0.0.1:{PORT}/devtools/page/1");

    axum::serve(listener, app).await.expect("server error");
}

#[link(name = "System", kind = "dylib")]
extern "C" {
    fn dispatch_main() -> !;
}
