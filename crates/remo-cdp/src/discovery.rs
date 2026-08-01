//! HTTP discovery (`/json`, `/json/version`, `/json/list`) — the same
//! endpoints Chrome itself queries for `chrome://inspect`, so a Remo target
//! shows up there with zero custom tooling.
//!
//! URLs are built from the request's `Host` header (not a hardcoded port),
//! so they stay correct through a USB port forward: the forwarded local port
//! on the Mac must appear in the URL, not whatever port the on-device
//! listener actually bound to.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;

/// axum 0.8 dropped the `Host` extractor from core (it now lives only in
/// `axum-extra`, not a dependency here) — read the raw header instead. Falls
/// back to `127.0.0.1` with no port if the header is somehow absent; every
/// real HTTP/1.1 client sends it, so this only matters for a malformed probe.
fn host_header(headers: &HeaderMap) -> String {
    headers
        .get(http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("127.0.0.1")
        .to_string()
}

/// Closes T-015 (no protocol version/handshake today): `remoProtocolVersion`
/// rides in the `/json/version` discovery response, which `chrome://inspect`
/// and any CDP client already fetch before opening a WebSocket. A generic CDP
/// client ignores the unknown field; a Remo-aware client (the thin CLI, the
/// MCP server) can compare it and refuse to talk to an incompatible server
/// instead of failing confusingly deeper in. There is deliberately no
/// preamble frame on the WebSocket itself — real Chrome DevTools sends CDP
/// methods immediately on connect and knows nothing about a Remo handshake.
pub const REMO_PROTOCOL_VERSION: &str = "1.0";

#[derive(Clone)]
pub struct DiscoveryConfig {
    /// Shown as the inspectable page's title — e.g. "Remo <device name>", so
    /// multiple simulators/devices are distinguishable in chrome://inspect.
    pub page_title: String,
    /// Stable id for the one target this process exposes. Remo's model is
    /// one app process = one inspectable target, so this never needs to
    /// change at runtime.
    pub page_id: String,
}

pub fn router(config: DiscoveryConfig) -> Router {
    Router::new()
        .route("/json/version", get(version))
        .route("/json", get(list))
        .route("/json/list", get(list))
        .with_state(Arc::new(config))
}

async fn version(headers: HeaderMap, State(config): State<Arc<DiscoveryConfig>>) -> Json<Value> {
    let host = host_header(&headers);
    Json(json!({
        "Browser": format!("Remo/{}", config.page_title),
        "Protocol-Version": "1.3",
        "remoProtocolVersion": REMO_PROTOCOL_VERSION,
        "webSocketDebuggerUrl": debugger_url(&host, &config.page_id),
        // Self-describing on purpose: a client (human or agent) that only
        // ever fetches this one well-known discovery endpoint — the same
        // one it needs anyway to get `webSocketDebuggerUrl` — can find the
        // zero-install `Remo.invoke` path (see `console.rs`) without reading
        // any external docs first.
        "remoConsoleUrl": console_url(&host),
    }))
}

async fn list(headers: HeaderMap, State(config): State<Arc<DiscoveryConfig>>) -> Json<Value> {
    let host = host_header(&headers);
    let ws_path = format!("{host}/devtools/page/{}", config.page_id);
    let debugger_url = format!("ws://{ws_path}");
    let frontend_url = format!("/devtools/inspector.html?ws={ws_path}");
    Json(json!([{
        "id": config.page_id,
        "type": "page",
        "title": config.page_title,
        "description": "",
        "url": "remo://app",
        "devtoolsFrontendUrl": frontend_url,
        "webSocketDebuggerUrl": debugger_url,
    }]))
}

fn debugger_url(host: &str, page_id: &str) -> String {
    format!("ws://{host}/devtools/page/{page_id}")
}

fn console_url(host: &str) -> String {
    format!("http://{host}/console")
}
