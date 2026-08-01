//! Routes CDP methods to domains and measures what they cost.
//!
//! Mirrors `DebugCDPDispatcher.swift` from the Gist-iOS reference work this
//! plan is built on: handlers are the only place expensive work happens, the
//! frontend is happy to ask the same thing dozens of times in a row, and a
//! regression here is invisible unless something is tallying it. One
//! `Dispatcher` (with its own `EventSink`) is constructed fresh per accepted
//! WebSocket connection and discarded on disconnect — Remo's target is one
//! app process, one client at a time, not a multiplexed many-session model.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};
use tracing::warn;

/// One inbound CDP request.
#[derive(Debug, Clone)]
pub struct CdpRequest {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

/// A domain's answer to one request.
#[derive(Debug, Clone)]
pub enum CdpReply {
    Result(Value),
    Error { code: i32, message: String },
}

impl CdpReply {
    pub fn ok(value: Value) -> Self {
        Self::Result(value)
    }

    pub fn empty() -> Self {
        Self::Result(Value::Object(Default::default()))
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            code: -32000,
            message: message.into(),
        }
    }
}

/// The single outbound-frame conduit for one connection — every reply *and*
/// every unsolicited event funnels through here, so there is exactly one
/// writer touching the WebSocket sink (mirrors `sendJSON` in the Swift
/// reference: replies and events are not two independently-synchronized
/// paths). Domains that need per-connection state (e.g. "is the screencast
/// currently on") own that state themselves; the sink only knows how to
/// queue frames.
#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::UnboundedSender<Value>,
}

impl EventSink {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Emits `{"method": ..., "params": ...}` — an unsolicited, no-`id`
    /// event. Silently drops if the connection already closed — the
    /// receiver is gone by then.
    pub fn emit(&self, method: &str, params: &Value) {
        self.emit_raw(serde_json::json!({
            "method": method,
            "params": params,
        }));
    }

    /// Queues an already-shaped frame — used by the transport layer to send
    /// `{id, result}`/`{id, error}` replies through the same funnel.
    pub fn emit_raw(&self, frame: Value) {
        let _ = self.tx.send(frame);
    }
}

/// A group of CDP methods backed by one native resource.
///
/// Domains compute answers and emit their own events; they never touch the
/// WebSocket directly. That keeps connection/session lifecycle in one place
/// (the dispatcher + transport), which is what lets every domain reuse the
/// same instrumentation without re-deriving it.
#[async_trait]
pub trait CdpDomain: Send + Sync {
    /// Exact method names this domain answers.
    fn methods(&self) -> &'static [&'static str];

    /// Answer one request. Domains are constructed once per connection, so
    /// `&self` is the right place for per-connection state (e.g. "is
    /// `Page.startScreencast` currently active") — see `domain_page`.
    async fn respond(&self, request: &CdpRequest, events: &EventSink) -> CdpReply;

    /// Connection closing — release anything time-bound (streaming tasks,
    /// registered watchers). Default no-op.
    fn reset(&self) {}
}

#[derive(Default)]
struct Stat {
    count: u64,
    total_micros: u64,
    max_micros: u64,
}

/// A single answer slower than this earns a log line: handlers may run
/// synchronously with respect to the connection's read loop, so a slow
/// answer is felt as app jank, not just debugger latency. Matches the
/// threshold used in the Swift reference implementation.
const SLOW_REQUEST_MICROS: u64 = 50_000;

pub struct Dispatcher {
    routes: HashMap<&'static str, Arc<dyn CdpDomain>>,
    domains: Vec<Arc<dyn CdpDomain>>,
    stats: Mutex<HashMap<String, Stat>>,
    next_synthetic_id: AtomicU64,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
            domains: Vec::new(),
            stats: Mutex::new(HashMap::new()),
            next_synthetic_id: AtomicU64::new(1),
        }
    }

    /// Registers a domain for every method it claims.
    ///
    /// Panics on a method claimed by two domains — that's a wiring bug at
    /// startup, not a runtime condition to handle gracefully (mirrors the
    /// `assert` in the Swift dispatcher).
    pub fn register(&mut self, domain: Arc<dyn CdpDomain>) {
        for method in domain.methods() {
            let previous = self.routes.insert(method, Arc::clone(&domain));
            assert!(
                previous.is_none(),
                "two domains claim {method}; the later one would win silently"
            );
        }
        self.domains.push(domain);
    }

    /// Returns `None` when no domain claims the method — the caller should
    /// generic-ack it (`{}`), matching every CDP method that's a pure ack
    /// (`*.enable`, `Overlay.setShow*`, etc.) with no domain-specific meaning.
    pub async fn dispatch(&self, request: CdpRequest, events: &EventSink) -> Option<CdpReply> {
        let domain = self.routes.get(request.method.as_str())?.clone();
        let start = Instant::now();
        let reply = domain.respond(&request, events).await;
        self.record(&request.method, start.elapsed().as_micros() as u64)
            .await;
        Some(reply)
    }

    /// A synthetic id for events the dispatcher itself needs to correlate,
    /// distinct from client-issued request ids.
    pub fn next_synthetic_id(&self) -> u64 {
        self.next_synthetic_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Connection closing: release every domain's per-connection state and
    /// log what the session cost.
    pub async fn reset(&self) {
        for domain in &self.domains {
            domain.reset();
        }
        self.log_session_stats().await;
    }

    async fn record(&self, method: &str, micros: u64) {
        if micros >= SLOW_REQUEST_MICROS {
            warn!(
                method,
                ms = micros / 1000,
                "slow CDP method on the main dispatch path"
            );
        }
        let mut stats = self.stats.lock().await;
        let stat = stats.entry(method.to_string()).or_default();
        stat.count += 1;
        stat.total_micros += micros;
        stat.max_micros = stat.max_micros.max(micros);
    }

    async fn log_session_stats(&self) {
        let stats = self.stats.lock().await;
        if stats.is_empty() {
            return;
        }
        let requests: u64 = stats.values().map(|s| s.count).sum();
        let total_ms: u64 = stats.values().map(|s| s.total_micros).sum::<u64>() / 1000;
        tracing::info!(requests, total_ms, "cdp session ended");
        let mut ranked: Vec<_> = stats.iter().collect();
        ranked.sort_by(|a, b| b.1.total_micros.cmp(&a.1.total_micros));
        for (method, stat) in ranked.into_iter().take(5) {
            tracing::info!(
                method,
                count = stat.count,
                total_ms = stat.total_micros / 1000,
                max_ms = stat.max_micros / 1000,
                "  method cost"
            );
        }
    }
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}
