//! The `Remo.*` domain — Track A of the rewrite plan, and the actual product.
//!
//! Every other domain in this crate (`domain_page`, `domain_dom`) exists to
//! make a *real* Chrome DevTools frontend draw a panel: `Page`, `DOM`, `CSS`,
//! `Overlay` are all names Chrome's own UI knows how to render. `Remo` is
//! not one of those names. That is fine, and it is the point.
//!
//! CDP itself does not require a client to be Chrome's frontend, or a domain
//! to be one Chrome ships. The wire protocol is just JSON-RPC-shaped frames
//! over a WebSocket (`{"id","method","params"}` in, `{"id","result"}` /
//! `{"id","error"}` / `{"method","params"}` out) plus an HTTP discovery
//! endpoint that hands out that WebSocket URL. Nothing about the transport,
//! the framing, or the dispatcher in this crate cares whether `method` is
//! `Page.navigate` or `Remo.invoke` — see [`CdpDomain`] and [`Dispatcher`]
//! in `dispatcher.rs`, which route purely by method-name string. A domain
//! Chrome's frontend doesn't recognize is simply a domain Chrome's frontend
//! won't draw a panel for; it is exactly as reachable, and exactly as valid
//! CDP, as `Page.reload`.
//!
//! What makes this the *product* rather than a curiosity is who the client
//! is meant to be: not `chrome://inspect`, but a thin, purpose-built client
//! — a CLI, an MCP server, an agent script — that speaks two methods:
//!
//! - `Remo.listCapabilities` — "what can I invoke?"
//! - `Remo.invoke` — "invoke this one, with these arguments."
//!
//! "Capability" here is deliberately open-ended: Remo's real app registers
//! whatever developer-named, typed actions it wants to expose (`navigate`,
//! `grid.feed.append`, and so on — see the plan doc for the motivating
//! examples). This crate does not know the app's capability names, does not
//! validate their argument shapes, and does not care how many there are; it
//! only knows how to shuttle a `(name, args)` pair to *something* that does
//! know, and shuttle the answer back. That "something" is [`CapabilityInvoker`].
//!
//! # Why the seam, not a direct dependency
//!
//! Today (pre-rewrite) the real capability store is `CapabilityRegistry` in
//! `remo-sdk` (`crates/remo-sdk/src/registry.rs`) — a `DashMap` of boxed
//! async handlers with `register`/`invoke`/`list`, plus a broadcast event on
//! change. Phase 1 of the rewrite plan has `remo-sdk` depend on `remo-cdp`
//! (to gain the new wire format), which makes the reverse dependency
//! (`remo-cdp` -> `remo-sdk`) a cycle — impossible in Cargo, and not
//! something we'd want even if it were possible, since it would make this
//! standalone crate (see the crate-level docs in `lib.rs`) reach back into
//! the rest of the workspace.
//!
//! [`CapabilityInvoker`] is the seam that avoids that: `remo-cdp` defines
//! the trait and depends on nothing to use it; a later phase implements the
//! trait *for* `CapabilityRegistry` (or a thin adapter around it) over in
//! `remo-sdk`, where that dependency direction is already established. This
//! module ships [`InMemoryCapabilities`] purely to prove the domain works in
//! isolation — a real handler table, not a mock, just not the production one.

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::{json, Value};

use crate::dispatcher::{CdpDomain, CdpReply, CdpRequest, EventSink};

/// The seam between this crate's CDP plumbing and whatever actually holds
/// named, invokable capabilities.
///
/// Kept deliberately small and synchronous-shaped in its return type (no
/// domain-specific error enum) so that a future `remo-sdk` adapter over
/// `CapabilityRegistry` — whose handlers are genuinely `async` and whose
/// errors are a richer `HandlerError` — can implement this trait by
/// flattening both into `Result<Value, String>` without contorting its own
/// types. If a future caller needs the richer error taxonomy back, that can
/// grow here later; starting narrow keeps this crate from guessing at
/// `remo-sdk`'s error shape today.
#[async_trait]
pub trait CapabilityInvoker: Send + Sync {
    /// Invokes the named capability with `params` as its arguments.
    ///
    /// - `None` means no capability is registered under `name` — distinct
    ///   from `Some(Err(_))`, which means the capability exists but the call
    ///   itself failed (bad args, handler-internal error, etc.).
    /// - `Some(Ok(value))` is the capability's JSON result.
    /// - `Some(Err(message))` is a human-readable failure description, not
    ///   a structured error — good enough for a CDP `error.message`.
    async fn invoke(&self, name: &str, params: Value) -> Option<Result<Value, String>>;

    /// Every capability name currently invokable, in no particular order.
    fn list(&self) -> Vec<String>;
}

/// The `Remo.*` CDP domain: `Remo.invoke` and `Remo.listCapabilities`,
/// backed by any [`CapabilityInvoker`].
///
/// Generic (rather than `Arc<dyn CapabilityInvoker>`) so that the concrete
/// invoker type is known at the call site and callers who only ever plug in
/// one implementation (the common case: one process, one registry) don't
/// pay for a vtable indirection they don't need. Nothing here requires that,
/// though — `RemoDomain<Arc<dyn CapabilityInvoker>>` also works, since
/// `Arc<dyn CapabilityInvoker>` itself is `Send + Sync + 'static` and this
/// module does not implement `CapabilityInvoker` specially for `dyn` trait
/// objects one way or the other.
pub struct RemoDomain<I: CapabilityInvoker + 'static> {
    invoker: Arc<I>,
}

impl<I: CapabilityInvoker + 'static> RemoDomain<I> {
    /// Wraps `invoker` as a CDP domain.
    pub fn new(invoker: Arc<I>) -> Self {
        Self { invoker }
    }
}

#[async_trait]
impl<I: CapabilityInvoker + 'static> CdpDomain for RemoDomain<I> {
    fn methods(&self) -> &'static [&'static str] {
        &["Remo.invoke", "Remo.listCapabilities"]
    }

    async fn respond(&self, request: &CdpRequest, _events: &EventSink) -> CdpReply {
        match request.method.as_str() {
            "Remo.listCapabilities" => CdpReply::ok(json!({ "names": self.invoker.list() })),
            "Remo.invoke" => self.invoke(request).await,
            other => CdpReply::error(format!("Remo domain does not handle {other}")),
        }
    }
}

impl<I: CapabilityInvoker + 'static> RemoDomain<I> {
    async fn invoke(&self, request: &CdpRequest) -> CdpReply {
        let Some(name) = request.params.get("name").and_then(Value::as_str) else {
            return CdpReply::error("Remo.invoke requires a string \"name\" param");
        };
        let args = request.params.get("args").cloned().unwrap_or(Value::Null);

        match self.invoker.invoke(name, args).await {
            None => CdpReply::error(format!("no such capability: {name}")),
            Some(Ok(value)) => CdpReply::ok(json!({ "result": value })),
            Some(Err(message)) => CdpReply::error(message),
        }
    }
}

/// A single synchronous capability handler: takes the call's `args` and
/// returns its JSON result or a human-readable failure message.
type SyncHandler = Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>;

/// A synchronous, in-memory capability table used to prove [`RemoDomain`]
/// works end to end without pulling in `remo-sdk`.
///
/// This is a demo fixture, not the production path: real capabilities (in
/// `remo-sdk`'s `CapabilityRegistry`) are async, may do real I/O, and emit a
/// `capabilities_changed` event on registration/removal. This type does
/// none of that — it seeds nothing on its own; `examples/standalone.rs`
/// registers its own `ping` capability to demonstrate the round trip.
#[derive(Default)]
pub struct InMemoryCapabilities {
    handlers: DashMap<String, SyncHandler>,
}

impl InMemoryCapabilities {
    /// An empty capability table.
    pub fn new() -> Self {
        Self {
            handlers: DashMap::new(),
        }
    }

    /// Registers a synchronous handler under `name`, replacing any existing
    /// handler of the same name.
    pub fn register(
        &self,
        name: impl Into<String>,
        handler: impl Fn(Value) -> Result<Value, String> + Send + Sync + 'static,
    ) {
        self.handlers.insert(name.into(), Arc::new(handler));
    }
}

#[async_trait]
impl CapabilityInvoker for InMemoryCapabilities {
    async fn invoke(&self, name: &str, params: Value) -> Option<Result<Value, String>> {
        let handler = self.handlers.get(name)?;
        let handler = Arc::clone(handler.value());
        Some(handler(params))
    }

    fn list(&self) -> Vec<String> {
        self.handlers
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::CdpReply;

    fn request(method: &str, params: Value) -> CdpRequest {
        CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
        }
    }

    fn domain_with_ping() -> RemoDomain<InMemoryCapabilities> {
        let capabilities = InMemoryCapabilities::new();
        capabilities.register("ping", |_args| Ok(json!({ "pong": true })));
        RemoDomain::new(Arc::new(capabilities))
    }

    #[tokio::test]
    async fn invoke_known_capability_returns_wrapped_result() {
        let domain = domain_with_ping();
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(
                &request("Remo.invoke", json!({ "name": "ping", "args": {} })),
                &events,
            )
            .await;

        match reply {
            CdpReply::Result(value) => assert_eq!(value, json!({ "result": { "pong": true } })),
            CdpReply::Error { message, .. } => panic!("expected success, got error: {message}"),
        }
    }

    #[tokio::test]
    async fn invoke_unknown_capability_is_an_error() {
        let domain = domain_with_ping();
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(
                &request(
                    "Remo.invoke",
                    json!({ "name": "does.not.exist", "args": {} }),
                ),
                &events,
            )
            .await;

        match reply {
            CdpReply::Error { message, .. } => {
                assert!(message.contains("does.not.exist"), "message was: {message}");
            }
            CdpReply::Result(value) => panic!("expected error, got result: {value}"),
        }
    }

    #[tokio::test]
    async fn invoke_propagates_handler_error() {
        let capabilities = InMemoryCapabilities::new();
        capabilities.register("boom", |_args| Err("kaboom".to_string()));
        let domain = RemoDomain::new(Arc::new(capabilities));
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(
                &request("Remo.invoke", json!({ "name": "boom", "args": {} })),
                &events,
            )
            .await;

        match reply {
            CdpReply::Error { message, .. } => assert_eq!(message, "kaboom"),
            CdpReply::Result(value) => panic!("expected error, got result: {value}"),
        }
    }

    #[tokio::test]
    async fn list_capabilities_reflects_registered_names() {
        let capabilities = InMemoryCapabilities::new();
        capabilities.register("a", |_| Ok(Value::Null));
        capabilities.register("b", |_| Ok(Value::Null));
        let domain = RemoDomain::new(Arc::new(capabilities));
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(&request("Remo.listCapabilities", Value::Null), &events)
            .await;

        match reply {
            CdpReply::Result(value) => {
                let mut names: Vec<String> = value["names"]
                    .as_array()
                    .expect("names should be an array")
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect();
                names.sort();
                assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
            }
            CdpReply::Error { message, .. } => panic!("expected success, got error: {message}"),
        }
    }

    #[tokio::test]
    async fn invoke_with_missing_name_is_a_clear_error_not_a_panic() {
        let domain = domain_with_ping();
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(&request("Remo.invoke", json!({ "args": {} })), &events)
            .await;

        match reply {
            CdpReply::Error { message, .. } => {
                assert!(message.contains("name"), "message was: {message}");
            }
            CdpReply::Result(value) => panic!("expected error, got result: {value}"),
        }
    }

    #[tokio::test]
    async fn invoke_with_non_string_name_is_a_clear_error_not_a_panic() {
        let domain = domain_with_ping();
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(
                &request("Remo.invoke", json!({ "name": 42, "args": {} })),
                &events,
            )
            .await;

        match reply {
            CdpReply::Error { message, .. } => {
                assert!(message.contains("name"), "message was: {message}");
            }
            CdpReply::Result(value) => panic!("expected error, got result: {value}"),
        }
    }

    #[tokio::test]
    async fn invoke_with_missing_args_defaults_to_null() {
        let capabilities = InMemoryCapabilities::new();
        capabilities.register("echo", |args| Ok(json!({ "echoed": args })));
        let domain = RemoDomain::new(Arc::new(capabilities));
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(&request("Remo.invoke", json!({ "name": "echo" })), &events)
            .await;

        match reply {
            CdpReply::Result(value) => {
                assert_eq!(value, json!({ "result": { "echoed": Value::Null } }));
            }
            CdpReply::Error { message, .. } => panic!("expected success, got error: {message}"),
        }
    }
}
