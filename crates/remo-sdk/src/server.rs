use std::net::SocketAddr;
use std::sync::Arc;

use remo_transport::Listener;
use tokio::sync::{broadcast, oneshot};
use tracing::{error, info};

use crate::registry::CapabilityRegistry;

/// Builds the CDP router for one connection. A closure, not a fixed
/// `Router`, because `remo_cdp::domain_page`/`domain_dom` hold
/// per-connection state (an in-flight screencast session, a node-id table)
/// that must not leak across clients — see `remo_cdp::transport::DispatcherFactory`.
async fn build_cdp_app(registry: CapabilityRegistry) -> axum::Router {
    let build_dispatcher: remo_cdp::transport::DispatcherFactory = Arc::new(move || {
        let mut dispatcher = remo_cdp::dispatcher::Dispatcher::new();
        dispatcher.register(Arc::new(crate::cdp_adapter::remo_domain(registry.clone())));
        dispatcher.register(Arc::new(remo_cdp::domain_page::PageDomain::new()));
        dispatcher.register(Arc::new(remo_cdp::domain_dom::DomDomain::new()));
        dispatcher
    });
    axum::Router::new()
        .merge(remo_cdp::discovery::router(
            remo_cdp::discovery::DiscoveryConfig {
                page_title: format!("Remo {}", device_name().await),
                page_id: "1".to_string(),
            },
        ))
        .merge(remo_cdp::transport::router(build_dispatcher))
}

/// Best-effort device name for the discovery title, so multiple
/// simulators/devices are distinguishable in `chrome://inspect`. Same
/// spawn_blocking treatment as the other UIKit-touching capabilities —
/// this runs inside a per-connection async task, not on tokio's main thread,
/// so `run_on_main_sync` must not be called from it directly.
#[allow(unsafe_code)]
async fn device_name() -> String {
    tokio::task::spawn_blocking(|| {
        remo_objc::run_on_main_sync(|| {
            // SAFETY: run_on_main_sync ensures main-thread execution.
            unsafe { remo_objc::get_device_info() }.name
        })
    })
    .await
    .unwrap_or_else(|_| "device".to_string())
}

/// The embedded RPC server running inside the iOS app.
pub struct RemoServer {
    registry: CapabilityRegistry,
    port: u16,
    shutdown_tx: broadcast::Sender<()>,
}

impl RemoServer {
    pub fn new(registry: CapabilityRegistry, port: u16) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);

        register_builtins(&registry);

        Self {
            registry,
            port,
            shutdown_tx,
        }
    }

    /// Return a clone of the shutdown sender for external use.
    pub fn shutdown_handle(&self) -> broadcast::Sender<()> {
        self.shutdown_tx.clone()
    }

    /// Start accepting connections. Blocks until shutdown.
    ///
    /// If `port_tx` is provided, sends the actual bound port once listening.
    /// This is essential when `port` is 0 (OS-assigned dynamic port).
    ///
    /// Every accepted connection is served as CDP (HTTP discovery + WS
    /// upgrade) — this dropped the dual-stack peek that used to route a
    /// connection to the old length-prefixed codec instead. That codec, the
    /// clients that spoke it (`remo-desktop`, `remo-daemon`), and the
    /// `capabilities_changed`-over-legacy-wire event forwarding this method
    /// used to wire up are gone (Phase 3 cutover of the rewrite plan) — see
    /// the plan's "Phase 3 — cut over" for why this is safe to do as a flag
    /// day now, unlike Phase 1's dual-stack requirement.
    pub async fn run(
        &self,
        port_tx: Option<oneshot::Sender<u16>>,
    ) -> Result<(), remo_transport::TransportError> {
        // Loopback only, deliberately — not `0.0.0.0`. A real device is
        // reached over its own USB-tunneled connection (usbmuxd), never
        // directly over Wi-Fi/LAN; binding every interface would expose an
        // unauthenticated port with no benefit. This matches the posture
        // real CDP servers already take (see the rewrite plan's Security
        // posture section) rather than carrying the old bind-all-interfaces
        // default over unexamined.
        let addr: SocketAddr = ([127, 0, 0, 1], self.port).into();
        let listener = Listener::bind(addr).await?;
        let actual_port = listener.local_addr().port();
        info!(port = actual_port, "remo server started");

        if let Some(tx) = port_tx {
            let _ = tx.send(actual_port);
        }

        loop {
            let mut shutdown_rx = self.shutdown_tx.subscribe();

            tokio::select! {
                result = listener.accept_raw() => {
                    match result {
                        Ok((stream, peer)) => {
                            let registry = self.registry.clone();
                            let mut shutdown_rx = self.shutdown_tx.subscribe();
                            tokio::spawn(async move {
                                info!(%peer, "accepted connection");
                                let app = build_cdp_app(registry).await;
                                tokio::select! {
                                    _ = remo_cdp::dual_stack::serve_on_stream(stream, app) => {}
                                    _ = shutdown_rx.recv() => {
                                        info!("cdp connection handler shutting down");
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            error!("accept error: {e}");
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("remo server shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Signal the server to shut down.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

// ---------------------------------------------------------------------------
// Built-in capabilities
// ---------------------------------------------------------------------------

#[allow(unsafe_code)]
fn register_builtins(registry: &CapabilityRegistry) {
    let reg = registry.clone();
    registry.register_sync("__list_capabilities", move |_| {
        Ok(serde_json::json!(reg.list()))
    });

    registry.register_sync("__ping", |_| Ok(serde_json::json!({"pong": true})));

    // `__view_tree`/`__screenshot` used to live here, duplicating what real
    // CDP already provides better: `DOM.getDocument` backs the actual
    // Elements panel (live, inspectable, highlightable — not a JSON dump),
    // and `Page.captureScreenshot` is what `remo screenshot`/`remo-mcp`
    // already call directly (see `remo-cdp`'s `domain_dom`/`domain_page`).
    // Keeping a Track-A capability that re-derives the same
    // `remo_objc::snapshot_view_tree()`/`capture_screenshot()` data was
    // redundant once Track B existed — removed rather than carried forward
    // unexamined.
    //
    // The two capabilities below still call into UIKit/ObjC via
    // `remo_objc::run_on_main_sync`, which blocks the calling thread until
    // the main thread services it. `register_sync`'s closures run wherever
    // `registry.invoke()` happens to be awaited from — today that is a
    // tokio task spawned per connection, i.e. a worker thread. Blocking a
    // worker thread until another thread (main) is free is exactly the
    // starvation hazard the rewrite plan's FFI section calls out: enough
    // concurrent requests can exhaust tokio's worker pool with threads
    // parked waiting on the same single main thread. `register` (the async
    // form) plus `spawn_blocking` moves that wait onto tokio's dedicated
    // blocking-thread pool instead, so worker threads stay free regardless
    // of how many of these are in flight at once.
    registry.register("__device_info", |_| async move {
        let info = tokio::task::spawn_blocking(|| {
            remo_objc::run_on_main_sync(|| {
                // SAFETY: run_on_main_sync ensures main-thread execution.
                unsafe { remo_objc::get_device_info() }
            })
        })
        .await
        .ok();
        Ok(crate::registry::HandlerOutput::Json(
            serde_json::to_value(info).unwrap_or_default(),
        ))
    });

    registry.register("__app_info", |_| async move {
        let info = tokio::task::spawn_blocking(|| {
            remo_objc::run_on_main_sync(|| {
                // SAFETY: run_on_main_sync ensures main-thread execution.
                unsafe { remo_objc::get_app_info() }
            })
        })
        .await
        .ok();
        Ok(crate::registry::HandlerOutput::Json(
            serde_json::to_value(info).unwrap_or_default(),
        ))
    });
}
