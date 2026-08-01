//! `remo-mcp` — the agent-facing companion to the thin CLI (Phase 2 of the
//! CDP rewrite plan). Exposes exactly two MCP tools, each a thin proxy onto
//! the `Remo.*` CDP domain implemented in `remo-cdp`:
//!
//! - `list_capabilities`  -> `Remo.listCapabilities`
//! - `invoke_capability`  -> `Remo.invoke` with `{"name", "args"}`
//!
//! There used to be a third, `get_view_tree` (`Remo.invoke` with the
//! now-removed `__view_tree` built-in) — dropped because it duplicated what
//! real CDP already does better: `DOM.getDocument` backs the actual Elements
//! panel (live, inspectable, highlightable), not a static JSON dump of the
//! same `remo_objc::snapshot_view_tree()` data under a different name. An
//! agent that needs the view hierarchy should drive `chrome://inspect`/a
//! `devtools://` URL directly (any browser-automation tool already does
//! this), not go through this proxy.
//!
//! This crate deliberately knows nothing about capability semantics or app
//! state — it only knows how to dial a `ws://` URL, send one CDP request,
//! and hand back the JSON result or a clear tool error. All business logic
//! (the capability registry, bootstrap-shape quirks) lives in
//! `remo-sdk`/`remo-cdp`, which this crate does not depend on and must not
//! reimplement.
//!
//! # Why a fresh WebSocket connection per call
//!
//! Each tool invocation dials the target's `ws://` URL, sends one request,
//! reads the one matching reply, then closes. This is the simplest correct
//! option for a thin proxy: MCP tool calls can arrive concurrently, and a
//! shared long-lived connection would need its own request-id-keyed
//! reply-routing table (exactly the complexity this crate exists to avoid
//! taking on) to multiplex them safely. Dialing fresh per call keeps this
//! crate's own logic trivial — connect, send, read, disconnect — at the
//! cost of one extra WebSocket handshake per call, which is a non-issue for
//! a debug/inspection tool. Document this trade-off here rather than leave
//! it implicit: if per-call handshake latency ever matters, the fix is a
//! connection-pooling layer *in front of* this crate (matching the plan's
//! own framing of `remo-daemon`-style pooling as a separable, addable
//! layer), not a change to this crate's proxying logic.
//!
//! # Configuration
//!
//! The target's `ws://` URL is resolved, in order:
//! 1. The first CLI argument, if provided: `remo-mcp ws://127.0.0.1:9930/devtools/page/1`
//! 2. The `REMO_WS_URL` environment variable.
//! 3. The default, `ws://127.0.0.1:9930/devtools/page/1` — matching the port
//!    and page id used by `remo-cdp`'s own `examples/standalone.rs` demo
//!    fixture, the easiest target to point this at for a manual smoke test.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Default target: matches `crates/remo-cdp/examples/standalone.rs`'s
/// `PORT` (9930) and its one page id ("1") — the easiest fixture to smoke
/// test this against, since it needs no real device/simulator.
const DEFAULT_WS_URL: &str = "ws://127.0.0.1:9930/devtools/page/1";

/// How long to wait for a single CDP reply before giving up and returning a
/// tool error. Generous, since some capabilities (e.g. ones that touch a
/// real device over usbmuxd) may be slower than a loopback round trip.
const CALL_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InvokeCapabilityRequest {
    /// The capability's registered name, e.g. "navigate" or "grid.feed.append".
    name: String,
    /// The capability's arguments, as a JSON object. Defaults to `{}` if omitted.
    #[serde(default = "empty_object")]
    params: Value,
}

fn empty_object() -> Value {
    json!({})
}

/// Sends one `{"id","method","params"}` request over a freshly dialed
/// WebSocket connection to `url`, waits for the one reply with a matching
/// `id`, and returns its `result` (or a plain-text error assembled from the
/// CDP `error` envelope / any transport failure).
///
/// This is the entire proxy: no retries, no caching, no interpretation of
/// what `result` contains — that's left to the capability/domain on the
/// other end.
async fn call_remo(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let (mut ws, _response) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|error| format!("failed to connect to {url}: {error}"))?;

    // Single in-flight request per connection, so any fixed id is fine —
    // there is nothing else to disambiguate against.
    let id = 1u64;
    let request = json!({ "id": id, "method": method, "params": params });
    ws.send(WsMessage::Text(request.to_string().into()))
        .await
        .map_err(|error| format!("failed to send {method} to {url}: {error}"))?;

    let next_message = tokio::time::timeout(CALL_TIMEOUT, ws.next());
    let message = match next_message.await {
        Ok(Some(Ok(message))) => message,
        Ok(Some(Err(error))) => return Err(format!("websocket error reading reply: {error}")),
        Ok(None) => return Err(format!("connection to {url} closed before a reply arrived")),
        Err(_elapsed) => return Err(format!("timed out waiting for a reply to {method}")),
    };

    let text = match message {
        WsMessage::Text(text) => text,
        other => return Err(format!("expected a text frame, got: {other:?}")),
    };

    let reply: Value = serde_json::from_str(&text)
        .map_err(|error| format!("reply was not valid JSON ({error}): {text}"))?;

    if let Some(error) = reply.get("error") {
        return Err(format!("{method} failed: {error}"));
    }

    Ok(reply.get("result").cloned().unwrap_or(Value::Null))
}

/// The MCP server: three tools, all proxying to the same `Remo.*` CDP
/// target dialed at `ws_url`.
#[derive(Clone)]
struct RemoMcp {
    ws_url: Arc<String>,
    tool_router: ToolRouter<Self>,
}

#[tool_router(router = tool_router)]
impl RemoMcp {
    fn new(ws_url: String) -> Self {
        Self {
            ws_url: Arc::new(ws_url),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List every capability name currently invokable on the connected Remo target (proxies Remo.listCapabilities)."
    )]
    async fn list_capabilities(&self) -> Result<String, String> {
        let result = call_remo(&self.ws_url, "Remo.listCapabilities", json!({})).await?;
        Ok(result.to_string())
    }

    #[tool(
        description = "Invoke a named, developer-registered capability on the connected Remo target with the given JSON params, and return its JSON result (proxies Remo.invoke)."
    )]
    async fn invoke_capability(
        &self,
        Parameters(InvokeCapabilityRequest { name, params }): Parameters<InvokeCapabilityRequest>,
    ) -> Result<String, String> {
        let result = call_remo(
            &self.ws_url,
            "Remo.invoke",
            json!({ "name": name, "args": params }),
        )
        .await?;
        Ok(result.to_string())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RemoMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            format!(
                "Thin proxy onto a Remo CDP target at {}. Call list_capabilities to discover \
                 what's invokable and invoke_capability(name, params) to run one. For the \
                 app's view hierarchy, drive chrome://inspect or a devtools:// URL directly \
                 (real CDP's DOM.getDocument) rather than this proxy. \
                 Override the target with the REMO_WS_URL env var or a CLI arg.",
                self.ws_url
            ),
        )
    }
}

fn resolve_ws_url() -> String {
    if let Some(arg) = std::env::args().nth(1) {
        return arg;
    }
    if let Ok(env_url) = std::env::var("REMO_WS_URL") {
        return env_url;
    }
    DEFAULT_WS_URL.to_string()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let ws_url = resolve_ws_url();
    tracing::info!(ws_url, "starting remo-mcp, proxying to Remo CDP target");

    let service = RemoMcp::new(ws_url)
        .serve(stdio())
        .await
        .inspect_err(|error| {
            tracing::error!(%error, "serving error");
        })?;

    service.waiting().await?;
    Ok(())
}
