//! WebSocket upgrade + the read/write loop that ties [`Dispatcher`] to the
//! wire. Single client at a time, matching Remo's one-target-per-process
//! model and the Swift reference implementation this plan is built on: a
//! fresh [`Dispatcher`] (and every domain's per-connection state) is
//! constructed per accepted connection and discarded when it closes.
//!
//! Exactly one task ever writes to the socket. Both request replies and
//! domain-emitted events funnel through the same [`EventSink`] queue into
//! that task — two independent writers racing on one `WebSocket` sink is
//! exactly the kind of bug that's invisible until two things happen to fire
//! at once, so it's structurally impossible here instead of merely unlikely.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Path;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tracing::{debug, warn};

use crate::dispatcher::{CdpReply, CdpRequest, Dispatcher, EventSink};

/// Builds the domains for one connection. A closure rather than a fixed
/// list because domains that hold per-connection state (screencast session,
/// registered file watchers) must be constructed fresh per connection, not
/// shared across clients.
pub type DispatcherFactory = Arc<dyn Fn() -> Dispatcher + Send + Sync>;

pub fn router(build_dispatcher: DispatcherFactory) -> Router {
    Router::new().route(
        "/devtools/page/{id}",
        get(move |ws: WebSocketUpgrade, Path(id): Path<String>| {
            let build_dispatcher = Arc::clone(&build_dispatcher);
            async move { upgrade(ws, id, build_dispatcher) }
        }),
    )
}

fn upgrade(ws: WebSocketUpgrade, page_id: String, build_dispatcher: DispatcherFactory) -> Response {
    ws.on_upgrade(move |socket| handle_connection(socket, page_id, build_dispatcher))
}

async fn handle_connection(
    socket: WebSocket,
    page_id: String,
    build_dispatcher: DispatcherFactory,
) {
    debug!(page_id, "devtools client connected");
    let dispatcher = build_dispatcher();
    let (events, mut outbound) = EventSink::new();
    let (mut sink, mut stream) = socket.split();

    // The only writer. Replies (pushed via `events.emit_raw` below) and
    // domain-emitted events (pushed via `events.emit`) both arrive here.
    let writer = tokio::spawn(async move {
        while let Some(frame) = outbound.recv().await {
            if sink
                .send(Message::Text(frame.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    while let Some(message) = stream.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, "devtools read error");
                break;
            }
        };
        let Message::Text(text) = message else {
            continue;
        };
        let Some(request) = parse_request(&text) else {
            warn!(%text, "unparseable CDP request");
            continue;
        };
        let reply = dispatcher.dispatch(request.clone(), &events).await;
        let envelope = match reply {
            Some(CdpReply::Result(value)) => {
                serde_json::json!({ "id": request.id, "result": value })
            }
            Some(CdpReply::Error { code, message }) => {
                serde_json::json!({ "id": request.id, "error": { "code": code, "message": message } })
            }
            // Not claimed by any domain: generic-ack. This is safe ONLY for
            // methods that are pure `enable`/`setShow*`-style no-ops — any
            // method whose reply shape the frontend actually reads must be
            // claimed by a domain instead. See the bootstrap-shape table in
            // domain_page.
            None => serde_json::json!({ "id": request.id, "result": {} }),
        };
        events.emit_raw(envelope);
    }

    dispatcher.reset().await;
    writer.abort();
    debug!(page_id, "devtools client disconnected");
}

fn parse_request(text: &str) -> Option<CdpRequest> {
    let value: Value = serde_json::from_str(text).ok()?;
    Some(CdpRequest {
        id: value.get("id")?.as_u64()?,
        method: value.get("method")?.as_str()?.to_string(),
        params: value
            .get("params")
            .cloned()
            .unwrap_or(Value::Object(Default::default())),
    })
}
