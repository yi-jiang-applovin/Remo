use std::net::SocketAddr;
use std::sync::Arc;

use remo_protocol::{ErrorCode, Event, Message, Request, Response};
use remo_transport::{Connection, Listener};
use tokio::sync::{broadcast, oneshot, Mutex};
use tracing::{error, info, warn};

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

        // Create the event broadcast channel and wire it into the registry
        // so register/unregister emit capabilities_changed events.
        /// Max queued events per subscriber before lagging.
        const EVENT_CHANNEL_CAPACITY: usize = 64;
        let (event_tx, _) = broadcast::channel::<Event>(EVENT_CHANNEL_CAPACITY);
        self.registry.set_event_sender(event_tx.clone());

        if let Some(tx) = port_tx {
            let _ = tx.send(actual_port);
        }

        loop {
            let mut shutdown_rx = self.shutdown_tx.subscribe();

            tokio::select! {
                // Dual-stack, not a flag day: peek the first bytes of every
                // accepted connection *before* committing it to either
                // protocol. Old framing starts with a 4-byte length prefix;
                // new (CDP) framing starts with an HTTP request line — see
                // `remo_cdp::dual_stack` for exactly how that's told apart
                // and why the two can never collide in practice. Existing
                // clients (today's `remo-cli`, `scripts/e2e-test.sh`) never
                // see a behavior change; only newly-arriving CDP clients
                // (Chrome, a rewritten CLI, an agent script) take the new
                // path.
                result = listener.accept_raw() => {
                    match result {
                        Ok((stream, peer)) => {
                            let registry = self.registry.clone();
                            let mut shutdown_rx = self.shutdown_tx.subscribe();
                            let event_rx = event_tx.subscribe();
                            tokio::spawn(async move {
                                let is_cdp = match remo_cdp::dual_stack::looks_like_http(&stream).await {
                                    Ok(is_cdp) => is_cdp,
                                    Err(e) => {
                                        warn!(%peer, "peek error: {e}");
                                        return;
                                    }
                                };

                                if is_cdp {
                                    info!(%peer, "accepted connection (CDP)");
                                    let app = build_cdp_app(registry).await;
                                    tokio::select! {
                                        _ = remo_cdp::dual_stack::serve_on_stream(stream, app) => {}
                                        _ = shutdown_rx.recv() => {
                                            info!("cdp connection handler shutting down");
                                        }
                                    }
                                    return;
                                }

                                info!(%peer, "accepted connection (legacy)");
                                let conn = match Connection::new(stream) {
                                    Ok(conn) => conn,
                                    Err(e) => {
                                        warn!(%peer, "legacy codec setup error: {e}");
                                        return;
                                    }
                                };
                                tokio::select! {
                                    _ = handle_connection(conn, registry, event_rx) => {}
                                    _ = shutdown_rx.recv() => {
                                        info!("connection handler shutting down");
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

    // The four capabilities below all call into UIKit/ObjC via
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
    registry.register("__view_tree", |params| async move {
        let depth: Option<usize> = params
            .get("max_depth")
            .and_then(serde_json::Value::as_u64)
            .map(|d| d as usize);

        let tree = tokio::task::spawn_blocking(move || {
            remo_objc::run_on_main_sync(|| {
                // SAFETY: run_on_main_sync ensures main-thread execution.
                let full_tree = unsafe { remo_objc::snapshot_view_tree() };
                full_tree.map(|t| {
                    if let Some(max) = depth {
                        truncate_tree(t, max, 0)
                    } else {
                        t
                    }
                })
            })
        })
        .await
        .unwrap_or_default();

        Ok(crate::registry::HandlerOutput::Json(
            serde_json::to_value(tree).unwrap_or_default(),
        ))
    });

    registry.register("__screenshot", |params| async move {
        let format = params
            .get("format")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("jpeg")
            .to_string();
        let quality = params
            .get("quality")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.8);

        let result = tokio::task::spawn_blocking(move || {
            remo_objc::run_on_main_sync(|| {
                // SAFETY: run_on_main_sync ensures main-thread execution.
                unsafe { remo_objc::capture_screenshot(&format, quality) }
            })
        })
        .await
        .unwrap_or_default();

        match result {
            Some(sr) => Ok(crate::registry::HandlerOutput::Binary {
                metadata: serde_json::json!({
                    "format": sr.format,
                    "width": sr.width,
                    "height": sr.height,
                    "scale": sr.scale,
                    "size": sr.bytes.len(),
                }),
                data: sr.bytes,
            }),
            None => Err(crate::registry::HandlerError::Internal(
                "screenshot capture failed".into(),
            )),
        }
    });

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

fn truncate_tree(
    mut node: remo_objc::ViewNode,
    max_depth: usize,
    current: usize,
) -> remo_objc::ViewNode {
    if current >= max_depth {
        let count = count_descendants(&node);
        node.children.clear();
        if count > 0 {
            node.class_name = format!("{} (+{count} children)", node.class_name);
        }
    } else {
        node.children = node
            .children
            .into_iter()
            .map(|c| truncate_tree(c, max_depth, current + 1))
            .collect();
    }
    node
}

fn count_descendants(node: &remo_objc::ViewNode) -> usize {
    node.children.len() + node.children.iter().map(count_descendants).sum::<usize>()
}

// ---------------------------------------------------------------------------
// Connection handling
// ---------------------------------------------------------------------------

async fn handle_connection(
    conn: Connection,
    registry: CapabilityRegistry,
    mut event_rx: broadcast::Receiver<Event>,
) {
    let peer = conn.peer_addr();
    info!(%peer, "handling connection");

    let (mut read_half, write_half) = conn.split();
    let write_half = Arc::new(Mutex::new(write_half));
    let sender = crate::streaming::StreamSender::new(Arc::clone(&write_half));

    // Spawn a task to forward capability change events to this client.
    let event_sender = sender.clone();
    let event_peer = peer;
    let event_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    if let Err(e) = event_sender.send_message(Message::Event(event)).await {
                        warn!(%event_peer, "event write error: {e}");
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(%event_peer, skipped = n, "event receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Active mirror session (only one at a time per connection)
    let mirror_session: Arc<Mutex<Option<Arc<crate::streaming::MirrorSession>>>> =
        Arc::new(Mutex::new(None));

    loop {
        let msg = match read_half.recv().await {
            Ok(Some(msg)) => msg,
            Ok(None) => {
                info!(%peer, "connection closed");
                break;
            }
            Err(e) => {
                warn!(%peer, "read error: {e}");
                break;
            }
        };

        match msg {
            Message::Request(req) => {
                let response_msg =
                    dispatch_request_with_streaming(&registry, req, &sender, &mirror_session).await;

                if let Err(e) = sender.send_message(response_msg).await {
                    warn!(%peer, "write error: {e}");
                    break;
                }
            }
            other => {
                warn!(%peer, "unexpected message type: {other:?}");
            }
        }
    }

    // Clean up the event forwarding task
    event_task.abort();

    // Stop any active mirror session on disconnect
    let session = mirror_session.lock().await.take();
    if let Some(s) = session {
        s.stop();
    }
}

async fn dispatch_request_with_streaming(
    registry: &CapabilityRegistry,
    req: Request,
    sender: &crate::streaming::StreamSender,
    mirror_session: &Arc<Mutex<Option<Arc<crate::streaming::MirrorSession>>>>,
) -> Message {
    let Request {
        id,
        capability,
        params,
    } = req;

    match capability.as_str() {
        "__start_mirror" => {
            let mut session_guard = mirror_session.lock().await;
            if session_guard.is_some() {
                return Message::Response(Response::error(
                    id,
                    ErrorCode::StreamAlreadyActive,
                    "a mirror stream is already active",
                ));
            }

            let fps = params
                .get("fps")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(30)
                .clamp(1, 120) as u32;

            let stream_id = 1u32;
            let session = Arc::new(crate::streaming::MirrorSession::new(stream_id));
            *session_guard = Some(Arc::clone(&session));

            let sender_clone = sender.clone();
            tokio::spawn(async move {
                crate::streaming::run_mirror_loop(session, sender_clone, fps).await;
            });

            Message::Response(Response::ok(
                id,
                serde_json::json!({ "stream_id": stream_id }),
            ))
        }
        "__stop_mirror" => {
            let mut session_guard = mirror_session.lock().await;
            if let Some(session) = session_guard.take() {
                session.stop();
                Message::Response(Response::ok(id, serde_json::json!({ "stopped": true })))
            } else {
                Message::Response(Response::error(
                    id,
                    ErrorCode::NotFound,
                    "no active mirror stream",
                ))
            }
        }
        _ => {
            dispatch_request(
                registry,
                Request {
                    id,
                    capability,
                    params,
                },
            )
            .await
        }
    }
}

async fn dispatch_request(registry: &CapabilityRegistry, req: Request) -> Message {
    let Request {
        id,
        capability,
        params,
    } = req;

    match registry.invoke(&capability, params).await {
        Some(Ok(output)) => match output {
            crate::registry::HandlerOutput::Json(data) => Message::Response(Response::ok(id, data)),
            crate::registry::HandlerOutput::Binary { metadata, data } => {
                Message::BinaryResponse(remo_protocol::BinaryResponse::new(id, metadata, data))
            }
        },
        Some(Err(e)) => {
            let code = match &e {
                crate::registry::HandlerError::InvalidParams(_) => ErrorCode::InvalidParams,
                crate::registry::HandlerError::Internal(_) => ErrorCode::Internal,
            };
            Message::Response(Response::error(id, code, e.to_string()))
        }
        None => Message::Response(Response::error(
            id,
            ErrorCode::NotFound,
            format!("capability '{capability}' not found"),
        )),
    }
}
