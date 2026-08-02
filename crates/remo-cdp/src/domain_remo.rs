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
//! # `Runtime.evaluate`/`Runtime.getProperties`: reaching Track A from the
//! # real Console panel
//!
//! Track B already needs nothing but Chrome itself — `chrome://inspect` or a
//! pasted `devtools://` URL. Track A had no equivalent: Chrome's own UI has
//! no button for an unrecognized domain, so reaching `Remo.invoke` used to
//! require a separate client (`remo-cli`, `remo-mcp`, or a hand-rolled
//! WebSocket script) even though the user was often *already* sitting in a
//! real DevTools window for the Elements/screencast panels.
//!
//! This domain also claims `Runtime.evaluate` (whatever you type into the
//! Console) and `Runtime.getProperties` (what backs the expand-arrow on a
//! printed object *and* Tab-completion as you type `remo.`). There is no real
//! JavaScript engine on the other end — the target is a native iOS app, not a
//! page — so `Runtime.evaluate` is not a JS interpreter; it is a small parser
//! for exactly one shape: `remo`, or a dotted chain of identifiers after it,
//! optionally followed by a single call with 0-1 JSON-object arguments —
//! `remo`, `remo.kv`, `remo.kv.delete`, `remo.kv.delete({"key": "x"})`.
//! A capability's own dots (`kv.delete`, `grid.tab.select` — this is
//! already the naming convention the rest of Remo uses) become real object
//! nesting: no separate `invoke(name, args)`/`listCapabilities()` indirection
//! to remember, and no special discovery step — evaluating bare `remo` (or
//! any dotted prefix of it) returns a real, `objectId`-bearing `RemoteObject`
//! whose children are computed live from [`CapabilityInvoker::list`], so
//! DevTools' own expand-arrow and Tab-completion "just work" against
//! whatever capabilities happen to be registered *right now* — nothing here
//! is a fixed schema.
//!
//! Anything else (an arbitrary JS expression, a typo, unquoted object keys)
//! returns a real `exceptionDetails` — DevTools renders it as a normal thrown
//! error in the Console, not a crash — with a message pointing back at the
//! supported grammar, rather than silently pretending to be a general JS
//! console. One more real-JS-engine behavior deliberately reproduced: a
//! `Runtime.evaluate` sent with `throwOnSideEffect: true` (DevTools' live
//! preview-while-typing, sent on every keystroke that completes a syntactically
//! valid expression, *before* Enter is pressed) always refuses to actually
//! invoke a capability — exactly like a real JS engine refuses to run
//! anything that might have a side effect under that flag. Without this, a
//! capability shaped like "delete this stored key" could fire while the user
//! is still mid-keystroke, never having pressed Enter at all.
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
use crate::remote_object::remote_object;

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
        &[
            "Remo.invoke",
            "Remo.listCapabilities",
            "Runtime.enable",
            "Runtime.evaluate",
            "Runtime.getProperties",
        ]
    }

    async fn respond(&self, request: &CdpRequest, events: &EventSink) -> CdpReply {
        match request.method.as_str() {
            "Remo.listCapabilities" => CdpReply::ok(json!({ "names": self.invoker.list() })),
            "Remo.invoke" => self.invoke(request).await,
            "Runtime.getProperties" => self.get_properties(request).await,
            "Runtime.enable" => {
                // A bare `{}` ack alone leaves the Console panel's context
                // selector on "Not selected" and disabled — DevTools only
                // considers the console usable once *some* execution context
                // exists. There is no real JS execution context here (the
                // target is a native app), but announcing one synthetic
                // "default" context is what the frontend is actually
                // watching for; this is the same bootstrap-event pattern as
                // `Storage.setStorageBucketTracking` needing a followup
                // `Storage.storageBucketCreatedOrUpdated` — an ack alone
                // isn't enough to unlock the feature that depends on it.
                events.emit(
                    "Runtime.executionContextCreated",
                    &json!({
                        "context": {
                            "id": REMO_EXECUTION_CONTEXT_ID,
                            "origin": "remo://native",
                            "name": "remo",
                            "uniqueId": "remo-native-context",
                            "auxData": { "isDefault": true, "type": "default" },
                        },
                    }),
                );
                CdpReply::empty()
            }
            "Runtime.evaluate" => self.evaluate(request).await,
            other => CdpReply::error(format!("Remo domain does not handle {other}")),
        }
    }
}

/// The one synthetic execution context this domain ever announces — Remo's
/// target is a single native app process, not a page that can navigate to a
/// new JS realm, so there is never a second context to create or a reason to
/// destroy this one mid-connection.
const REMO_EXECUTION_CONTEXT_ID: u64 = 1;

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

    /// Handles `Runtime.evaluate` — see the module docs for why this exists
    /// and exactly what grammar it accepts. Always replies with a shaped
    /// success (`CdpReply::ok`, never a CDP-level `CdpReply::error`): a
    /// failed evaluation is reported *inside* the reply as
    /// `exceptionDetails`, which is what tells DevTools' Console to render
    /// a normal red "Uncaught" line instead of surfacing a raw protocol
    /// error toast — the same distinction real `Runtime.evaluate` makes
    /// between "the call itself failed" and "the expression threw".
    async fn evaluate(&self, request: &CdpRequest) -> CdpReply {
        let Some(expression) = request.params.get("expression").and_then(Value::as_str) else {
            return CdpReply::ok(evaluate_exception(
                "Runtime.evaluate requires a string \"expression\" param",
            ));
        };
        let throw_on_side_effect = request
            .params
            .get("throwOnSideEffect")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        match parse_console_expr(expression) {
            Some(ConsoleExpr::Reference(path)) => {
                let names = self.invoker.list();
                CdpReply::ok(match classify(&path, &names) {
                    Node::Missing => evaluate_exception(&format!(
                        "remo.{path} is not a registered capability or namespace"
                    )),
                    Node::Capability => json!({ "result": function_remote_object(&path) }),
                    Node::Namespace(children) => {
                        json!({ "result": namespace_remote_object(&path, &children) })
                    }
                })
            }
            Some(ConsoleExpr::Invoke { name, args }) => {
                if throw_on_side_effect {
                    // Real V8 refuses to run anything with a possible side
                    // effect under this flag — DevTools sends it on every
                    // keystroke that completes a syntactically valid
                    // expression, to render a live preview *before* Enter is
                    // pressed. A capability call is exactly the kind of
                    // thing that must not fire mid-keystroke (imagine
                    // "kv.delete" firing because the user hasn't finished
                    // typing the closing paren yet).
                    return CdpReply::ok(side_effect_exception());
                }
                match self.invoker.invoke(&name, args).await {
                    None => {
                        CdpReply::ok(evaluate_exception(&format!("no such capability: {name}")))
                    }
                    Some(Ok(value)) => CdpReply::ok(evaluate_success(&value)),
                    Some(Err(message)) => CdpReply::ok(evaluate_exception(&message)),
                }
            }
            None => CdpReply::ok(evaluate_exception(&format!(
                "Remo's Console understands `remo` (browse registered capabilities/namespaces) \
                 and `remo.<dotted.capability.name>({{...}})` (call one) — this isn't a real JS \
                 engine, so arbitrary expressions aren't supported. Args must be strict JSON \
                 (double-quoted keys/strings). Got: {expression}"
            ))),
        }
    }

    /// Handles `Runtime.getProperties` — what backs the expand-arrow on a
    /// printed `remo`/namespace object, and (via the same mechanism real
    /// DevTools uses) Tab-completion while typing `remo.`. `objectId`s are
    /// entirely self-describing (`remo:<dotted.path>`) and never cached —
    /// every call recomputes children from whatever
    /// [`CapabilityInvoker::list`] returns *right now*, so the browsable
    /// tree never goes stale relative to the app's actual registered
    /// capabilities.
    async fn get_properties(&self, request: &CdpRequest) -> CdpReply {
        let Some(object_id) = request.params.get("objectId").and_then(Value::as_str) else {
            return CdpReply::error("Runtime.getProperties requires a string \"objectId\" param");
        };
        let Some(path) = object_id.strip_prefix(OBJECT_ID_PREFIX) else {
            return CdpReply::error(format!("unrecognized objectId: {object_id}"));
        };

        let names = self.invoker.list();
        let children = children_of(path, &names);
        let descriptors: Vec<Value> = children
            .iter()
            .map(|(name, kind)| {
                let child_path = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}.{name}")
                };
                let value = match kind {
                    ChildKind::Namespace => {
                        namespace_remote_object(&child_path, &children_of(&child_path, &names))
                    }
                    ChildKind::Capability => function_remote_object(&child_path),
                };
                json!({
                    "name": name,
                    "value": value,
                    "writable": false,
                    "configurable": false,
                    "enumerable": true,
                })
            })
            .collect();

        CdpReply::ok(json!({ "result": descriptors }))
    }
}

/// A parsed `remo`/`remo.*` console expression — the entire grammar this
/// domain understands, deliberately not a general JS parser (see module
/// docs): either a bare dotted reference (browsing) or a single call with
/// 0-1 JSON-object arguments (invoking).
enum ConsoleExpr {
    /// `remo`, `remo.kv`, `remo.kv.delete` (no trailing call) — a
    /// reference to browse, empty string for bare `remo` itself.
    Reference(String),
    /// `remo.<dotted.name>(<json object>?)` — an actual capability call.
    Invoke { name: String, args: Value },
}

/// How one path relates to what's currently registered — computed fresh from
/// [`CapabilityInvoker::list`] on every call, never cached, so it can never
/// go stale relative to capabilities registering/unregistering at runtime.
enum Node {
    /// Neither an exact capability name nor a prefix of one.
    Missing,
    /// `path` is itself an exact, invokable capability name.
    Capability,
    /// One or more capability names live under this dotted prefix.
    /// `children`: immediate next segments, alphabetically sorted.
    Namespace(Vec<(String, ChildKind)>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildKind {
    Namespace,
    Capability,
}

/// Classifies `path` against the live capability list — the answer to "what
/// does `remo` or `remo.<path>` mean right now?"
fn classify(path: &str, names: &[String]) -> Node {
    let children = children_of(path, names);
    if !children.is_empty() {
        return Node::Namespace(children);
    }
    if path.is_empty() {
        return Node::Namespace(Vec::new()); // an empty root is still a valid (empty) object
    }
    if names.iter().any(|n| n == path) {
        return Node::Capability;
    }
    Node::Missing
}

/// The immediate next dotted segment of every capability name under `path`
/// (or every top-level segment, for the root `path == ""`), deduplicated and
/// classified as a further namespace (something is registered even deeper)
/// or a leaf capability. A segment that is both an exact capability name
/// *and* has deeper capabilities under it (e.g. both "kv" and "kv.sub"
/// registered) is classified as a namespace — browsing deeper is more useful
/// than treating it as a dead-end function.
fn children_of(path: &str, names: &[String]) -> Vec<(String, ChildKind)> {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}.")
    };
    let mut namespaces = std::collections::BTreeSet::new();
    let mut leaves = std::collections::BTreeSet::new();
    for name in names {
        let Some(rest) = name.strip_prefix(prefix.as_str()) else {
            continue;
        };
        if rest.is_empty() {
            continue; // name == path exactly; that's Node::Capability's concern, not a child
        }
        match rest.split_once('.') {
            Some((first, _)) => {
                namespaces.insert(first.to_string());
            }
            None => {
                leaves.insert(rest.to_string());
            }
        }
    }
    let mut children: Vec<(String, ChildKind)> = namespaces
        .into_iter()
        .map(|n| (n, ChildKind::Namespace))
        .collect();
    for leaf in leaves {
        if !children.iter().any(|(n, _)| *n == leaf) {
            children.push((leaf, ChildKind::Capability));
        }
    }
    children.sort_by(|a, b| a.0.cmp(&b.0));
    children
}

/// Recognizes `remo`, a dotted chain of identifiers after it
/// (`remo.kv.delete`), and optionally a single trailing call with 0-1
/// JSON-object arguments. Whitespace around dots and the outer call is
/// tolerated; call arguments must be strict JSON (this is intentionally less
/// lenient than real JS — see the module docs for why).
fn parse_console_expr(expression: &str) -> Option<ConsoleExpr> {
    let trimmed = expression.trim();
    let mut cursor = trimmed.strip_prefix("remo")?;
    let mut segments: Vec<&str> = Vec::new();

    loop {
        let after_ws = cursor.trim_start();
        let Some(after_dot) = after_ws.strip_prefix('.') else {
            cursor = after_ws;
            break;
        };
        let after_dot = after_dot.trim_start();
        let ident_len = after_dot
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after_dot.len());
        if ident_len == 0 {
            return None; // a dot with no identifier after it (e.g. mid-autocomplete "remo.")
        }
        segments.push(&after_dot[..ident_len]);
        cursor = &after_dot[ident_len..];
    }

    let name = segments.join(".");
    if cursor.is_empty() {
        return Some(ConsoleExpr::Reference(name));
    }

    let inner = cursor.strip_prefix('(')?;
    let inner = inner.strip_suffix(';').unwrap_or(inner).trim_end();
    let inner = inner.strip_suffix(')')?;
    let mut args = split_top_level_args(inner)?;
    if args.len() > 1 {
        return None;
    }
    let args_json: Value = match args.pop() {
        Some(raw) if !raw.trim().is_empty() => serde_json::from_str(raw.trim()).ok()?,
        _ => Value::Object(Default::default()),
    };
    if name.is_empty() {
        return None; // "remo()" doesn't name a capability
    }
    Some(ConsoleExpr::Invoke {
        name,
        args: args_json,
    })
}

/// Splits a call's argument-list text into top-level comma-separated pieces,
/// respecting string literals and nested brackets/braces/parens so a comma
/// inside `{"a": [1, 2]}` doesn't split the object argument in two. Returns
/// `None` on unterminated strings or unbalanced brackets rather than
/// guessing.
fn split_top_level_args(text: &str) -> Option<Vec<&str>> {
    if text.trim().is_empty() {
        return Some(Vec::new());
    }

    let mut parts = Vec::new();
    let mut depth: i32 = 0;
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    let mut start = 0;

    for (i, ch) in text.char_indices() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => in_string = Some(ch),
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    if in_string.is_some() || depth != 0 {
        return None;
    }
    parts.push(&text[start..]);
    Some(parts)
}

/// A successful `Runtime.evaluate` reply: `{result: <RemoteObject>}`.
fn evaluate_success(value: &Value) -> Value {
    json!({ "result": remote_object(value) })
}

/// Every synthetic `objectId` this domain ever hands out starts with this —
/// `Runtime.getProperties` uses it to recognize its own ids and recover the
/// dotted path they refer to. Not a real allocated object anywhere: the id
/// *is* the path, so there is nothing to look up in a table and nothing to
/// leak if a client holds onto one past a capability's lifetime — the next
/// `Runtime.getProperties` call on it just recomputes against whatever is
/// registered by then.
const OBJECT_ID_PREFIX: &str = "remo:";

/// The `RemoteObject` for a namespace (`remo` itself, or any dotted prefix
/// with something registered under it) — has a real `objectId` so DevTools'
/// expand-arrow and Tab-completion can call `Runtime.getProperties` on it.
fn namespace_remote_object(path: &str, children: &[(String, ChildKind)]) -> Value {
    json!({
        "type": "object",
        "className": "Object",
        "description": "Object",
        "objectId": format!("{OBJECT_ID_PREFIX}{path}"),
        "preview": {
            "type": "object",
            "description": "Object",
            "overflow": false,
            "properties": children.iter().map(|(name, kind)| match kind {
                ChildKind::Namespace => json!({"name": name, "type": "object", "value": "Object"}),
                ChildKind::Capability => json!({"name": name, "type": "function", "value": "ƒ"}),
            }).collect::<Vec<_>>(),
        },
    })
}

/// The `RemoteObject` for a bare reference to an exact capability name with
/// no call attached (`remo.ping`, not `remo.ping()`) — shown as a function,
/// matching how a real method reference prints in a JS console. No
/// `objectId`: a function reference has no further properties worth
/// expanding here (no real `.call`/`.bind`/closure internals to show).
fn function_remote_object(full_name: &str) -> Value {
    // DevTools extracts the name it displays after the `ƒ` glyph by parsing
    // `description` as if it were real V8 function source — expecting
    // `function <name>(...) { ... }`, the same shape V8 itself produces for
    // a native/built-in function (e.g. `function values() { [native code] }`).
    // An earlier version of this used `"ƒ remo.{full_name}()"` directly,
    // which doesn't match that shape at all: DevTools' parser found nothing
    // it recognized as a name and rendered `ƒ undefined()` instead — a real
    // bug, found from a live screenshot, not a hypothetical. Using the
    // capability's last dotted segment as the declared name fixes the
    // display *and* the `{ ... }` body doubles as exactly the kind of
    // inline description a human inspecting the Console benefits from —
    // the full dotted capability name, so `select` under `grid.tab` still
    // reads unambiguously as `grid.tab.select`.
    let leaf = full_name.rsplit('.').next().unwrap_or(full_name);
    json!({
        "type": "function",
        "className": "Function",
        "description": format!("function {leaf}() {{ [remo capability: {full_name}] }}"),
    })
}

/// What DevTools gets back for a capability call attempted under
/// `throwOnSideEffect: true` — see the module docs and `evaluate`'s handling
/// of that flag. Shaped as a real thrown `EvalError`, matching what a real
/// JS engine returns for the same flag on a call it can't prove is
/// side-effect-free, so DevTools silently skips the live preview instead of
/// rendering anything (it does not display this exception to the user the
/// way a real `Uncaught` line would — this case is specifically the "don't
/// show a preview" signal, not a user-visible error).
fn side_effect_exception() -> Value {
    json!({
        "result": { "type": "undefined" },
        "exceptionDetails": {
            "exceptionId": 1,
            "text": "Uncaught",
            "lineNumber": 0,
            "columnNumber": 0,
            "exception": {
                "type": "object",
                "subtype": "error",
                "className": "EvalError",
                "description": "EvalError: Possible side effect in Remo capability call",
            },
        },
    })
}

/// A failed `Runtime.evaluate` reply shaped as a thrown exception, per the
/// CDP `Runtime.ExceptionDetails` shape — this is what makes DevTools'
/// Console render `message` as a normal red "Uncaught" line.
fn evaluate_exception(message: &str) -> Value {
    json!({
        "result": { "type": "undefined" },
        "exceptionDetails": {
            "exceptionId": 1,
            "text": "Uncaught",
            "lineNumber": 0,
            "columnNumber": 0,
            "exception": {
                "type": "object",
                "subtype": "error",
                "className": "Error",
                "description": message,
            },
        },
    })
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

    // -- Runtime.evaluate / Runtime.getProperties: the Console-panel grammar --

    fn domain_with_capabilities() -> RemoDomain<InMemoryCapabilities> {
        let capabilities = InMemoryCapabilities::new();
        capabilities.register("echo", |args| Ok(json!({ "echoed": args })));
        capabilities.register("ping", |_args| Ok(json!({ "pong": true })));
        capabilities.register("kv.delete", |args| Ok(json!({ "deleted": args["key"] })));
        capabilities.register("kv.get", |args| Ok(json!({ "value": args["key"] })));
        RemoDomain::new(Arc::new(capabilities))
    }

    /// `Runtime.evaluate`/`Runtime.getProperties` must always be a
    /// `CdpReply::ok` — even on failure, for evaluate — since a failed
    /// evaluation is reported via `exceptionDetails` inside a successful
    /// reply, not a CDP-level protocol error. DevTools reads this
    /// distinction to decide whether to show a red "Uncaught" console line
    /// or a different kind of failure entirely.
    fn expect_ok_result(reply: CdpReply) -> Value {
        match reply {
            CdpReply::Result(value) => value,
            CdpReply::Error { message, .. } => {
                panic!("expected CdpReply::ok, got an error: {message}")
            }
        }
    }

    async fn evaluate(domain: &RemoDomain<InMemoryCapabilities>, expression: &str) -> Value {
        let (events, _rx) = EventSink::new();
        expect_ok_result(
            domain
                .respond(
                    &request("Runtime.evaluate", json!({ "expression": expression })),
                    &events,
                )
                .await,
        )
    }

    #[tokio::test]
    async fn evaluate_dotted_call_reaches_the_real_capability() {
        let domain = domain_with_capabilities();
        let value = evaluate(&domain, r#"remo.kv.delete({"key": "foo"})"#).await;

        assert!(value.get("exceptionDetails").is_none(), "value: {value}");
        assert_eq!(
            value["result"]["preview"]["properties"][0]["name"],
            "deleted"
        );
    }

    #[tokio::test]
    async fn evaluate_call_without_args_defaults_to_empty_object() {
        let domain = domain_with_capabilities();
        let value = evaluate(&domain, "remo.ping()").await;

        assert!(value.get("exceptionDetails").is_none(), "value: {value}");
    }

    #[tokio::test]
    async fn evaluate_bare_remo_is_a_namespace_object_listing_top_level_children() {
        let domain = domain_with_capabilities();
        let value = evaluate(&domain, "remo").await;

        assert!(value.get("exceptionDetails").is_none(), "value: {value}");
        assert_eq!(value["result"]["objectId"], "remo:");
        let names: std::collections::BTreeSet<String> = value["result"]["preview"]["properties"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect();
        // "echo" and "ping" are leaves at the root; "kv" is a namespace
        // (kv.delete, kv.get), not the two dotted names directly.
        assert_eq!(
            names,
            ["echo", "ping", "kv"]
                .into_iter()
                .map(String::from)
                .collect()
        );
    }

    #[tokio::test]
    async fn evaluate_dotted_namespace_reference_lists_its_own_children() {
        let domain = domain_with_capabilities();
        let value = evaluate(&domain, "remo.kv").await;

        assert!(value.get("exceptionDetails").is_none(), "value: {value}");
        assert_eq!(value["result"]["objectId"], "remo:kv");
        let names: std::collections::BTreeSet<String> = value["result"]["preview"]["properties"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["delete", "get"].into_iter().map(String::from).collect()
        );
    }

    #[tokio::test]
    async fn evaluate_bare_leaf_capability_reference_is_a_function_not_a_call() {
        let domain = domain_with_capabilities();
        let value = evaluate(&domain, "remo.ping").await;

        assert!(value.get("exceptionDetails").is_none(), "value: {value}");
        assert_eq!(value["result"]["type"], "function");
        assert!(value["result"]["objectId"].is_null(), "value: {value}");
    }

    #[tokio::test]
    async fn evaluate_reference_to_nothing_registered_is_an_exception() {
        let domain = domain_with_capabilities();
        let value = evaluate(&domain, "remo.nope").await;

        let message = value["exceptionDetails"]["exception"]["description"]
            .as_str()
            .unwrap();
        assert!(message.contains("remo.nope"), "message: {message}");
    }

    #[tokio::test]
    async fn evaluate_unknown_capability_call_is_an_exception_not_a_protocol_error() {
        let domain = domain_with_capabilities();
        let value = evaluate(&domain, r#"remo.no.such.thing({})"#).await;

        let message = value["exceptionDetails"]["exception"]["description"]
            .as_str()
            .unwrap();
        assert!(message.contains("no.such.thing"), "message: {message}");
    }

    #[tokio::test]
    async fn evaluate_rejects_non_json_args_with_a_helpful_message_not_a_panic() {
        let domain = domain_with_capabilities();

        // Bare (unquoted) object keys are valid JS but not valid JSON —
        // deliberately unsupported (see the module docs); must fail
        // gracefully, not panic the dispatcher.
        let value = evaluate(&domain, r#"remo.echo({hello: "console"})"#).await;

        let message = value["exceptionDetails"]["exception"]["description"]
            .as_str()
            .unwrap();
        assert!(message.contains("strict JSON"), "message: {message}");
    }

    #[tokio::test]
    async fn evaluate_rejects_arbitrary_js_expressions_gracefully() {
        let domain = domain_with_capabilities();
        let value = evaluate(&domain, "1 + 1").await;

        assert!(
            value.get("exceptionDetails").is_some(),
            "expected an exception for an unsupported expression, got: {value}"
        );
    }

    #[tokio::test]
    async fn evaluate_rejects_a_trailing_expression_after_the_call() {
        let domain = domain_with_capabilities();
        let value = evaluate(&domain, r#"remo.ping(); remo.echo({})"#).await;

        assert!(
            value.get("exceptionDetails").is_some(),
            "a second statement after the call must not be silently ignored: {value}"
        );
    }

    #[tokio::test]
    async fn evaluate_under_throw_on_side_effect_never_actually_invokes() {
        let domain = domain_with_capabilities();
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(
                &request(
                    "Runtime.evaluate",
                    json!({
                        "expression": r#"remo.kv.delete({"key": "foo"})"#,
                        "throwOnSideEffect": true,
                    }),
                ),
                &events,
            )
            .await;

        let value = expect_ok_result(reply);
        let class_name = value["exceptionDetails"]["exception"]["className"]
            .as_str()
            .unwrap();
        assert_eq!(
            class_name, "EvalError",
            "a call under throwOnSideEffect must never actually reach the capability, value: {value}"
        );
    }

    #[tokio::test]
    async fn evaluate_under_throw_on_side_effect_still_allows_bare_references() {
        // Property/namespace access has no side effects in this model —
        // only calling a capability does — matching how a real JS engine
        // still allows plain property reads under this flag.
        let domain = domain_with_capabilities();
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(
                &request(
                    "Runtime.evaluate",
                    json!({ "expression": "remo.kv", "throwOnSideEffect": true }),
                ),
                &events,
            )
            .await;

        let value = expect_ok_result(reply);
        assert!(value.get("exceptionDetails").is_none(), "value: {value}");
    }

    #[tokio::test]
    async fn get_properties_on_the_root_lists_top_level_children() {
        let domain = domain_with_capabilities();
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(
                &request("Runtime.getProperties", json!({ "objectId": "remo:" })),
                &events,
            )
            .await;

        let value = expect_ok_result(reply);
        let names: std::collections::BTreeSet<String> = value["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["echo", "ping", "kv"]
                .into_iter()
                .map(String::from)
                .collect()
        );
        // "kv" must be a real, further-expandable object (its own
        // objectId), not a dead-end — that's what lets DevTools' expand
        // arrow and Tab-completion recurse into it.
        let kv = value["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "kv")
            .unwrap();
        assert_eq!(kv["value"]["objectId"], "remo:kv");
    }

    #[tokio::test]
    async fn get_properties_on_a_namespace_lists_its_leaves_as_functions() {
        let domain = domain_with_capabilities();
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(
                &request("Runtime.getProperties", json!({ "objectId": "remo:kv" })),
                &events,
            )
            .await;

        let value = expect_ok_result(reply);
        let entries = value["result"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        for entry in entries {
            assert_eq!(entry["value"]["type"], "function");
        }
    }

    #[tokio::test]
    async fn get_properties_rejects_an_unrecognized_object_id() {
        let domain = domain_with_capabilities();
        let (events, _rx) = EventSink::new();

        let reply = domain
            .respond(
                &request("Runtime.getProperties", json!({ "objectId": "not-ours" })),
                &events,
            )
            .await;

        match reply {
            CdpReply::Error { .. } => {}
            CdpReply::Result(value) => panic!("expected an error, got: {value}"),
        }
    }

    #[test]
    fn split_top_level_args_respects_nested_braces_and_strings() {
        let parts = split_top_level_args(r#""a", {"x": [1, 2], "y": "a, b"}"#).unwrap();
        assert_eq!(parts, vec![r#""a""#, r#" {"x": [1, 2], "y": "a, b"}"#]);
    }

    #[test]
    fn split_top_level_args_rejects_unbalanced_brackets() {
        assert!(split_top_level_args(r#""a", {"x": 1"#).is_none());
    }

    #[test]
    fn split_top_level_args_rejects_unterminated_strings() {
        assert!(split_top_level_args(r#""a"#).is_none());
    }
}
