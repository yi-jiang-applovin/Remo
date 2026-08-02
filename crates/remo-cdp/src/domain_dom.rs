//! The UIView tree, mirrored as Chrome DevTools' Elements panel — plus the
//! three `Page.*` methods that describe "the one document" this domain hangs
//! that tree off of, and a read-only, geometry-derived `CSS.*`/`Overlay.*`
//! surface alongside it.
//!
//! Ported from `DebugCDPDOMMirror.swift` / the `Page.*` half of
//! `DebugCDPStorageMirror.swift` (Gist-iOS `PulseDevTools`, this week's
//! from-scratch, empirically-debugged-against-real-Chrome reference). The
//! behavior is deliberately unchanged from that reference; only the shapes
//! are Rust's. Two v1 non-goals, carried over from the rewrite plan, not
//! silently dropped:
//!
//! - **No live DOM mutation events.** `remo-objc::snapshot_view_tree()`
//!   returns an owned snapshot, not a live view reference, so the tree is
//!   frozen as of the last `DOM.getDocument`/`DOM.requestChildNodes` call.
//!   Refresh (re-issue `DOM.getDocument`) to see updates.
//! - **No style editing.** UIKit has no cascade, no stylesheets, no
//!   `display`/`position` model — `CSS.getComputedStyleForNode` maps view
//!   geometry to pseudo-CSS name/value pairs, and that is the entire CSS
//!   surface. `CSS.setStyleTexts` and friends are not claimed.
//!
//! `Overlay.highlightNode`/`Overlay.hideHighlight` are also a known, called-out
//! gap: the Swift reference drew a real on-screen highlight view because it
//! ran in-process with direct UIKit access; this Rust crate has no equivalent
//! yet, so both methods are acknowledged (`CdpReply::empty()`) with no visible
//! effect. Selecting a node in the Elements panel will not highlight it on
//! the device until that's built.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::{json, Value};

use remo_objc::ViewNode;

use crate::dispatcher::{CdpDomain, CdpReply, CdpRequest, EventSink};
use crate::remote_object::remote_object;

/// The one "document" every `Page.*`/`DOM.*` reply hangs off of — there is
/// exactly one inspectable target (the app itself), so this is a fixed
/// string, not a discovered URL. Public because `domain_storage` maps
/// `Storage.getStorageKeyForFrame`'s main-frame case back onto this same
/// origin.
pub const MAIN_ORIGIN: &str = "remo://native";

/// Node-count (not depth) budget for the eager `DOM.getDocument` reply. A
/// depth cap tells you nothing on a deep-but-narrow tree (the Swift reference
/// measured a real screen at 501 nodes across 25 levels); a count budget is
/// the axis that actually protects against a huge tree while still being
/// useful on typical ones. Levels beyond the budget load lazily via
/// `DOM.requestChildNodes`.
const MAX_EAGER_NODES: i64 = 2000;

/// Node id 1 is reserved for `#document`; real view nodes start at 2.
const DOCUMENT_NODE_ID: u64 = 1;
const FIRST_VIEW_NODE_ID: u64 = 2;

/// `DOM`/`CSS`/`Overlay`/document-`Page.*`: the UIView tree as an Elements
/// panel.
///
/// Node ids are handed out by an incrementing counter and remembered in a
/// `DashMap<u64, ViewNode>` keyed by that id, so a later
/// `DOM.requestChildNodes`/`DOM.getBoxModel`/`DOM.resolveNode`/
/// `Overlay.highlightNode` can look the node back up. This differs from the
/// Swift reference, which held a *weak* reference to the live `UIView` (so a
/// deallocated view resolved to nil): `ViewNode` here is a plain, owned,
/// `Clone`-able snapshot with no live view behind it at all, so there is
/// nothing to weakly reference — the whole subtree from the last
/// `getDocument`/`requestChildNodes` just stays alive in the map until the
/// next `reset()` or the next `getDocument` clears it. The accepted
/// trade-off: a node id can point at a stale snapshot if the real view tree
/// changed since it was captured (matches the documented "tree is a
/// snapshot" non-goal); it can *never* point at a dangling/freed reference,
/// which is strictly safer than the Swift design, just less "live."
pub struct DomDomain {
    nodes: DashMap<u64, ViewNode>,
    next_node_id: AtomicU64,
}

impl DomDomain {
    pub fn new() -> Self {
        Self {
            nodes: DashMap::new(),
            next_node_id: AtomicU64::new(FIRST_VIEW_NODE_ID),
        }
    }

    /// Hands out a fresh id for `node` and remembers it, cloning the node
    /// (and thus its whole remaining subtree) into the table. Cheap enough
    /// for Phase 0: `ViewNode` is plain data, and clones only happen while
    /// walking a snapshot that was just taken.
    fn register(&self, node: &ViewNode) -> u64 {
        let id = self.next_node_id.fetch_add(1, Ordering::Relaxed);
        self.nodes.insert(id, node.clone());
        id
    }

    fn lookup(&self, id: u64) -> Option<ViewNode> {
        self.nodes.get(&id).map(|entry| entry.clone())
    }

    /// The frontend addresses a node by `nodeId` in some calls and
    /// `backendNodeId` in others (selection resolves by the latter). Ours are
    /// the same numbers, so accept either rather than failing the call and
    /// handing the frontend an `undefined` it will dereference.
    fn node_from_request(&self, request: &CdpRequest) -> Option<ViewNode> {
        if let Some(id) = request.params.get("nodeId").and_then(Value::as_u64) {
            if let Some(node) = self.lookup(id) {
                return Some(node);
            }
        }
        let backend_id = request
            .params
            .get("backendNodeId")
            .and_then(Value::as_u64)?;
        self.lookup(backend_id)
    }

    /// Serializes one view (and, budget/depth permitting, its subtree) into a
    /// CDP `Node` object, registering an id for every node it touches.
    ///
    /// `depth < 0` means "keep going" (matches real CDP's `-1` = whole
    /// subtree); `depth == 0` stops recursing but still returns this node
    /// with an honest `childNodeCount` so `DOM.requestChildNodes` has
    /// somewhere to expand from. `budget` is shared mutable state across the
    /// whole walk — the safety valve that a depth cap alone can't provide on
    /// a deep-but-narrow tree.
    fn node_json(&self, view: &ViewNode, parent_id: u64, depth: i64, budget: &mut i64) -> Value {
        let node_id = self.register(view);
        let mut children = Vec::new();
        if depth != 0 && *budget > 0 {
            *budget -= view.children.len() as i64;
            let child_depth = if depth < 0 { depth } else { depth - 1 };
            for child in &view.children {
                children.push(self.node_json(child, node_id, child_depth, budget));
            }
        }
        let mut node = json!({
            "nodeId": node_id,
            "parentId": parent_id,
            "backendNodeId": node_id,
            "nodeType": 1,
            "nodeName": view.class_name,
            "localName": view.class_name,
            "nodeValue": "",
            "attributes": attributes(view),
            "childNodeCount": view.children.len(),
        });
        if !children.is_empty() {
            node["children"] = Value::Array(children);
        }
        node
    }

    #[allow(unsafe_code)]
    async fn snapshot() -> Option<ViewNode> {
        // `run_on_main_sync` blocks the calling thread until the main thread
        // services it — never call it directly from a tokio worker (would
        // starve the executor's pool). `spawn_blocking` fences it onto a
        // thread tokio expects to block.
        tokio::task::spawn_blocking(|| {
            remo_objc::run_on_main_sync(|| {
                // SAFETY: `snapshot_view_tree` requires being called on the
                // main thread; `run_on_main_sync` guarantees this closure
                // runs there (dispatching via GCD if the calling thread
                // isn't already main).
                unsafe { remo_objc::snapshot_view_tree() }
            })
        })
        .await
        .unwrap_or(None)
    }

    async fn get_document(&self, request: &CdpRequest) -> CdpReply {
        let depth = request
            .params
            .get("depth")
            .and_then(Value::as_i64)
            .unwrap_or(-1);

        self.nodes.clear();
        self.next_node_id
            .store(FIRST_VIEW_NODE_ID, Ordering::Relaxed);

        let root_view = Self::snapshot().await;
        let mut budget = MAX_EAGER_NODES;
        let children = match &root_view {
            Some(view) => vec![self.node_json(view, DOCUMENT_NODE_ID, depth, &mut budget)],
            None => Vec::new(),
        };
        if budget <= 0 {
            tracing::warn!(
                max = MAX_EAGER_NODES,
                "view tree exceeded eager node budget; deeper levels load on DOM.requestChildNodes"
            );
        }

        let root = json!({
            "nodeId": DOCUMENT_NODE_ID,
            "backendNodeId": DOCUMENT_NODE_ID,
            "nodeType": 9,
            "nodeName": "#document",
            "localName": "",
            "nodeValue": "",
            "documentURL": MAIN_ORIGIN,
            "baseURL": MAIN_ORIGIN,
            "xmlVersion": "",
            "childNodeCount": children.len(),
            "children": children,
        });
        CdpReply::ok(json!({ "root": root }))
    }

    async fn request_child_nodes(&self, request: &CdpRequest, events: &EventSink) -> CdpReply {
        let Some(parent_id) = request.params.get("nodeId").and_then(Value::as_u64) else {
            return CdpReply::empty();
        };
        // Ack first, exactly like the Swift reference: `setChildNodes` is an
        // event, not the reply payload, and the frontend expects the ack
        // even when the parent id is unknown (e.g. a stale snapshot).
        let reply = CdpReply::empty();
        if let Some(parent) = self.lookup(parent_id) {
            let nodes: Vec<Value> = parent
                .children
                .iter()
                .map(|child| {
                    let mut budget = MAX_EAGER_NODES;
                    self.node_json(child, parent_id, 1, &mut budget)
                })
                .collect();
            events.emit(
                "DOM.setChildNodes",
                &json!({ "parentId": parent_id, "nodes": nodes }),
            );
        }
        reply
    }

    fn resolve_node(&self, request: &CdpRequest) -> CdpReply {
        let Some(view) = self.node_from_request(request) else {
            return CdpReply::error("no such node");
        };
        let described: Value = computed_style(&view)
            .into_iter()
            .map(|(name, value)| (name, Value::String(value)))
            .collect::<serde_json::Map<_, _>>()
            .into();
        let mut object = remote_object(&described);
        if let Value::Object(map) = &mut object {
            map.insert("subtype".to_string(), json!("node"));
            map.insert("className".to_string(), json!(view.class_name));
            map.insert("description".to_string(), json!(view.class_name));
        }
        CdpReply::ok(json!({ "object": object }))
    }

    fn get_box_model(&self, request: &CdpRequest) -> CdpReply {
        let Some(view) = self.node_from_request(request) else {
            return CdpReply::error("no such node");
        };
        // Known simplification vs. the Swift reference: `ViewNode.frame` is
        // the view's frame in its *own superview's* coordinate space (as
        // captured by the walker), not converted into window coordinates the
        // way `UIView.convert(_:to:)` did there. A nested view's box model
        // quad will therefore be offset from where it actually renders
        // on-screen. Flagged as a gap for later — fixing it means having the
        // walker accumulate absolute origin while it recurses.
        let f = &view.frame;
        let quad = vec![
            f.x,
            f.y,
            f.x + f.width,
            f.y,
            f.x + f.width,
            f.y + f.height,
            f.x,
            f.y + f.height,
        ];
        CdpReply::ok(json!({
            "model": {
                "content": quad,
                "padding": quad,
                "border": quad,
                "margin": quad,
                "width": f.width,
                "height": f.height,
            }
        }))
    }

    fn push_nodes_by_backend_ids(&self, request: &CdpRequest) -> CdpReply {
        // Our backendNodeIds and nodeIds are the same numbers.
        let ids: Vec<u64> = request
            .params
            .get("backendNodeIds")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        let known: Vec<u64> = ids
            .into_iter()
            .filter(|id| self.nodes.contains_key(id))
            .collect();
        CdpReply::ok(json!({ "nodeIds": known }))
    }

    fn get_computed_style_for_node(&self, request: &CdpRequest) -> CdpReply {
        let Some(view) = self.node_from_request(request) else {
            return CdpReply::error("no such node");
        };
        let style: Vec<Value> = computed_style(&view)
            .into_iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect();
        CdpReply::ok(json!({ "computedStyle": style }))
    }
}

impl Default for DomDomain {
    fn default() -> Self {
        Self::new()
    }
}

/// CDP wants attributes as a flat `[name, value, name, value, ...]`.
fn attributes(view: &ViewNode) -> Vec<String> {
    let mut attrs = Vec::new();
    if let Some(id) = &view.accessibility_id {
        if !id.is_empty() {
            attrs.push("id".to_string());
            attrs.push(id.clone());
        }
    }
    if view.is_hidden {
        attrs.push("hidden".to_string());
        attrs.push("true".to_string());
    }
    if view.alpha < 1.0 {
        attrs.push("alpha".to_string());
        attrs.push(format!("{:.2}", view.alpha));
    }
    attrs.push("frame".to_string());
    attrs.push(format!(
        "{:.0},{:.0} {:.0}x{:.0}",
        view.frame.x, view.frame.y, view.frame.width, view.frame.height
    ));
    // Populated only for SwiftUI nodes spliced in by
    // `remo_objc::swiftui_debug` — see `ViewNode::modifiers`'s doc comment
    // for why this is a separate attribute rather than folded into
    // `nodeName`/`localName` (the tag name) the way it briefly was.
    if !view.modifiers.is_empty() {
        attrs.push("modifiers".to_string());
        attrs.push(view.modifiers.join(", "));
    }
    attrs
}

/// Geometry-derived, read-only "computed style" — the entire `CSS.*` surface
/// this domain has anything honest to offer for (see module doc: no cascade,
/// no stylesheets exist to mirror). Field list mirrors
/// `DebugCDPDOMMirror.computedStyle(of:)` verbatim, minus the fields the
/// Swift version read off `UIView` that `ViewNode` doesn't carry (background
/// color, corner radius, label text) — not a regression, `ViewNode` simply
/// doesn't capture those today.
fn computed_style(view: &ViewNode) -> Vec<(String, String)> {
    let mut style = vec![
        ("class".to_string(), view.class_name.clone()),
        ("left".to_string(), format!("{}px", view.frame.x as i64)),
        ("top".to_string(), format!("{}px", view.frame.y as i64)),
        (
            "width".to_string(),
            format!("{}px", view.frame.width as i64),
        ),
        (
            "height".to_string(),
            format!("{}px", view.frame.height as i64),
        ),
        ("opacity".to_string(), format!("{:.2}", view.alpha)),
        (
            "visibility".to_string(),
            if view.is_hidden { "hidden" } else { "visible" }.to_string(),
        ),
        ("tag".to_string(), view.tag.to_string()),
        ("subviews".to_string(), view.children.len().to_string()),
    ];
    // SwiftUI-only, populated by `remo_objc::swiftui_debug`'s bounded
    // properties flattener (see `ViewNode::style_rows`'s doc comment).
    // Prefixed like a CSS custom property (`--swiftui-...`) purely as a
    // naming convention to keep them visually distinct from the geometry-
    // derived pseudo-CSS above in Chrome's real Styles pane — this module
    // has no real cascade to enforce custom-property semantics for, same
    // as every other "pseudo-CSS" field here.
    for (title, value) in &view.style_rows {
        style.push((format!("--swiftui-{title}"), value.clone()));
    }
    style
}

#[async_trait]
impl CdpDomain for DomDomain {
    fn methods(&self) -> &'static [&'static str] {
        &[
            "Page.getResourceTree",
            "Page.getAppManifest",
            "Page.getNavigationHistory",
            "DOM.getDocument",
            "DOM.requestChildNodes",
            "DOM.resolveNode",
            "DOM.getBoxModel",
            "DOM.pushNodesByBackendIdsToFrontend",
            "CSS.getComputedStyleForNode",
            "CSS.getMatchedStylesForNode",
            "CSS.getInlineStylesForNode",
            "Overlay.highlightNode",
            "Overlay.hideHighlight",
        ]
    }

    async fn respond(&self, request: &CdpRequest, events: &EventSink) -> CdpReply {
        match request.method.as_str() {
            // The Application panel's storage sidebar discovers origins from
            // this frame tree, not from any `DOMStorage.*` method — so the
            // `userdefaults://standard` bucket `domain_storage::StorageDomain`
            // answers for is listed here as a child frame, matching the
            // Swift reference's `frameTree()`. Extending this to more
            // buckets (e.g. IndexedDB-backed ones) means adding more entries
            // to `childFrames`, not changing the shape.
            "Page.getResourceTree" => CdpReply::ok(json!({
                "frameTree": {
                    "frame": {
                        "id": "1",
                        "loaderId": "1",
                        "url": MAIN_ORIGIN,
                        "securityOrigin": MAIN_ORIGIN,
                        "mimeType": "text/html",
                        "domainAndRegistry": "",
                    },
                    "resources": [],
                    "childFrames": [{
                        "frame": {
                            "id": crate::domain_storage::USERDEFAULTS_ORIGIN,
                            "parentId": "1",
                            "loaderId": "1",
                            "name": crate::domain_storage::USERDEFAULTS_ORIGIN,
                            "url": format!("{}/", crate::domain_storage::USERDEFAULTS_ORIGIN),
                            "securityOrigin": crate::domain_storage::USERDEFAULTS_ORIGIN,
                            "mimeType": "text/html",
                            "domainAndRegistry": "",
                            "secureContextType": "Secure",
                            "crossOriginIsolatedContextType": "NotIsolated",
                            "gatedAPIFeatures": [],
                        },
                        "resources": [],
                    }],
                }
            })),
            // A bare `{}` makes `application.js` choke on `errors.length` and
            // abort Application-panel init before anything else loads — same
            // empirically-found bug as the Swift reference, same fix.
            "Page.getAppManifest" => CdpReply::ok(json!({ "url": MAIN_ORIGIN, "errors": [] })),
            "Page.getNavigationHistory" => CdpReply::ok(json!({
                "currentIndex": 0,
                "entries": [{
                    "id": 1,
                    "url": MAIN_ORIGIN,
                    "userTypedURL": MAIN_ORIGIN,
                    "title": "Remo",
                    "transitionType": "typed",
                }],
            })),
            "DOM.getDocument" => self.get_document(request).await,
            "DOM.requestChildNodes" => self.request_child_nodes(request, events).await,
            "DOM.resolveNode" => self.resolve_node(request),
            "DOM.getBoxModel" => self.get_box_model(request),
            "DOM.pushNodesByBackendIdsToFrontend" => self.push_nodes_by_backend_ids(request),
            "CSS.getComputedStyleForNode" => self.get_computed_style_for_node(request),
            // Must be exactly these 5 empty arrays or the Elements panel
            // throws "s is not iterable" and dies — confirmed empirically
            // against real Chrome 150 in the Swift reference. Do not
            // "simplify" this to a bare `{}`.
            "CSS.getMatchedStylesForNode" => CdpReply::ok(json!({
                "matchedCSSRules": [],
                "pseudoElements": [],
                "inherited": [],
                "inheritedPseudoElements": [],
                "cssKeyframesRules": [],
            })),
            // CSS.getInlineStylesForNode: both `inlineStyle`/`attributesStyle`
            // are optional per spec — an empty ack is a legal, honest answer
            // since there is no real inline-style source to report.
            "CSS.getInlineStylesForNode" => CdpReply::empty(),
            // No on-device visual highlight overlay from pure Rust/objc2 yet
            // (unlike the Swift reference, which drew a real overlay view via
            // direct in-process UIKit access) — acknowledge only. Known gap,
            // not a silent no-op: selecting/hovering a node in the Elements
            // panel will not visibly highlight anything on the device.
            "Overlay.highlightNode" | "Overlay.hideHighlight" => CdpReply::empty(),
            _ => CdpReply::error(format!("unhandled method: {}", request.method)),
        }
    }

    /// Fresh ids next session — matches "each DevTools session starts empty."
    fn reset(&self) {
        self.nodes.clear();
        self.next_node_id
            .store(FIRST_VIEW_NODE_ID, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remo_objc::Frame;

    fn leaf(name: &str) -> ViewNode {
        ViewNode {
            class_name: name.to_string(),
            frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0,
            },
            is_hidden: false,
            alpha: 1.0,
            tag: 0,
            accessibility_id: None,
            modifiers: Vec::new(),
            style_rows: Vec::new(),
            children: Vec::new(),
        }
    }

    fn parent_with(children: Vec<ViewNode>) -> ViewNode {
        ViewNode {
            class_name: "UIView".to_string(),
            frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 200.0,
            },
            is_hidden: false,
            alpha: 1.0,
            tag: 0,
            accessibility_id: Some("root".to_string()),
            modifiers: Vec::new(),
            style_rows: Vec::new(),
            children,
        }
    }

    fn request(method: &str, params: Value) -> CdpRequest {
        CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
        }
    }

    #[tokio::test]
    async fn matched_styles_is_exactly_five_empty_arrays() {
        let domain = DomDomain::new();
        let (events, _rx) = EventSink::new();
        let reply = domain
            .respond(&request("CSS.getMatchedStylesForNode", json!({})), &events)
            .await;
        let CdpReply::Result(value) = reply else {
            panic!("expected a result, got an error");
        };
        let obj = value.as_object().expect("object reply");
        assert_eq!(
            obj.len(),
            5,
            "must be exactly 5 keys or the Elements panel dies"
        );
        for key in [
            "matchedCSSRules",
            "pseudoElements",
            "inherited",
            "inheritedPseudoElements",
            "cssKeyframesRules",
        ] {
            assert_eq!(
                obj.get(key).and_then(Value::as_array).map(Vec::len),
                Some(0),
                "{key}"
            );
        }
    }

    #[tokio::test]
    async fn app_manifest_has_url_and_empty_errors() {
        let domain = DomDomain::new();
        let (events, _rx) = EventSink::new();
        let reply = domain
            .respond(&request("Page.getAppManifest", json!({})), &events)
            .await;
        let CdpReply::Result(value) = reply else {
            panic!("expected a result, got an error");
        };
        assert_eq!(value["url"], json!(MAIN_ORIGIN));
        assert_eq!(value["errors"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn navigation_history_has_one_entry_at_index_zero() {
        let domain = DomDomain::new();
        let (events, _rx) = EventSink::new();
        let reply = domain
            .respond(&request("Page.getNavigationHistory", json!({})), &events)
            .await;
        let CdpReply::Result(value) = reply else {
            panic!("expected a result, got an error");
        };
        assert_eq!(value["currentIndex"], json!(0));
        assert_eq!(value["entries"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn register_lookup_and_reset_round_trip() {
        let domain = DomDomain::new();
        let a = leaf("UILabel");
        let b = leaf("UIButton");
        let id_a = domain.register(&a);
        let id_b = domain.register(&b);
        assert_ne!(id_a, id_b);
        assert_eq!(
            domain.lookup(id_a).map(|n| n.class_name),
            Some("UILabel".to_string())
        );
        assert_eq!(
            domain.lookup(id_b).map(|n| n.class_name),
            Some("UIButton".to_string())
        );
        assert!(domain.lookup(999).is_none());

        CdpDomain::reset(&domain);
        assert!(domain.lookup(id_a).is_none());
        assert!(domain.lookup(id_b).is_none());
        // ids start fresh from FIRST_VIEW_NODE_ID again after reset.
        let id_after_reset = domain.register(&leaf("UIImageView"));
        assert_eq!(id_after_reset, FIRST_VIEW_NODE_ID);
    }

    #[test]
    fn node_json_registers_the_whole_walked_subtree() {
        let domain = DomDomain::new();
        let tree = parent_with(vec![leaf("UILabel"), leaf("UIButton")]);
        let mut budget = MAX_EAGER_NODES;
        let json = domain.node_json(&tree, DOCUMENT_NODE_ID, -1, &mut budget);
        assert_eq!(json["nodeName"], json!("UIView"));
        assert_eq!(json["childNodeCount"], json!(2));
        assert_eq!(json["children"].as_array().map(Vec::len), Some(2));
        // Budget only accounts for the 2 direct children in this small tree.
        assert_eq!(budget, MAX_EAGER_NODES - 2);
        // 1 id for the root + 2 for its children.
        assert_eq!(domain.nodes.len(), 3);
    }

    #[test]
    fn modifiers_surface_as_their_own_cdp_attribute_not_in_the_tag_name() {
        // Simulates what `swiftui_debug.rs` now produces: `class_name` is
        // just the view's own type, `modifiers` is separate — this is the
        // shape `attributes()` (and, one level up, `nodeName`/`localName`
        // in `node_json`) must keep apart, matching the split introduced to
        // stop unreadable multi-hundred-character SwiftUI tag names.
        let mut view = leaf("SwiftUI.VStack<TupleView<(Text, Text)>>");
        view.modifiers = vec![
            "_PaddingLayout".to_string(),
            "_BackgroundModifier<Color>".to_string(),
        ];

        let domain = DomDomain::new();
        let mut budget = MAX_EAGER_NODES;
        let json = domain.node_json(&view, DOCUMENT_NODE_ID, 0, &mut budget);

        assert_eq!(
            json["nodeName"],
            json!("SwiftUI.VStack<TupleView<(Text, Text)>>")
        );
        let attrs = json["attributes"].as_array().expect("attributes array");
        let idx = attrs
            .iter()
            .position(|v| v == "modifiers")
            .expect("modifiers attribute present");
        assert_eq!(
            attrs[idx + 1],
            json!("_PaddingLayout, _BackgroundModifier<Color>")
        );
    }

    #[test]
    fn modifiers_attribute_is_absent_for_plain_uikit_nodes() {
        let domain = DomDomain::new();
        let mut budget = MAX_EAGER_NODES;
        let json = domain.node_json(&leaf("UILabel"), DOCUMENT_NODE_ID, 0, &mut budget);
        let attrs = json["attributes"].as_array().expect("attributes array");
        assert!(!attrs.iter().any(|v| v == "modifiers"));
    }

    #[test]
    fn style_rows_surface_as_prefixed_pseudo_css_declarations() {
        // Simulates what `swiftui_debug.rs`'s bounded properties flattener
        // (Part B) produces: a flat (title, value) list, including a
        // trailing truncation-note row when the flattener's depth/row caps
        // were hit.
        let mut view = leaf("Text");
        view.style_rows = vec![
            ("font".to_string(), "title3".to_string()),
            ("…".to_string(), "3 more (truncated)".to_string()),
        ];

        let domain = DomDomain::new();
        let id = domain.register(&view);
        let reply = domain.get_computed_style_for_node(&request(
            "CSS.getComputedStyleForNode",
            json!({ "nodeId": id }),
        ));
        let CdpReply::Result(value) = reply else {
            panic!("expected a result, got an error");
        };
        let style = value["computedStyle"].as_array().expect("array");

        let font_row = style
            .iter()
            .find(|d| d["name"] == json!("--swiftui-font"))
            .expect("flattened style_rows entry present as its own pseudo-CSS declaration");
        assert_eq!(font_row["value"], json!("title3"));

        let truncation_row = style
            .iter()
            .find(|d| d["name"] == json!("--swiftui-…"))
            .expect("truncation-note row is surfaced too, not silently dropped");
        assert_eq!(truncation_row["value"], json!("3 more (truncated)"));

        // The existing geometry-derived pseudo-CSS is untouched alongside it.
        assert!(style.iter().any(|d| d["name"] == json!("left")));
    }

    #[test]
    fn no_style_rows_means_no_extra_pseudo_css_declarations() {
        let domain = DomDomain::new();
        let id = domain.register(&leaf("UILabel"));
        let reply = domain.get_computed_style_for_node(&request(
            "CSS.getComputedStyleForNode",
            json!({ "nodeId": id }),
        ));
        let CdpReply::Result(value) = reply else {
            panic!("expected a result, got an error");
        };
        let style = value["computedStyle"].as_array().expect("array");
        assert!(!style.iter().any(|d| d["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("--swiftui-"))));
    }

    #[test]
    fn depth_zero_stops_recursion_but_reports_child_count() {
        let domain = DomDomain::new();
        let tree = parent_with(vec![leaf("UILabel")]);
        let mut budget = MAX_EAGER_NODES;
        let json = domain.node_json(&tree, DOCUMENT_NODE_ID, 0, &mut budget);
        assert_eq!(json["childNodeCount"], json!(1));
        assert!(json.get("children").is_none());
        // Only the root itself was registered — the child was never walked.
        assert_eq!(domain.nodes.len(), 1);
    }

    #[test]
    fn resolve_node_shape_has_object_id_and_node_subtype() {
        let domain = DomDomain::new();
        let view = leaf("UILabel");
        let id = domain.register(&view);
        let reply = domain.resolve_node(&request("DOM.resolveNode", json!({ "nodeId": id })));
        let CdpReply::Result(value) = reply else {
            panic!("expected a result, got an error");
        };
        let object = &value["object"];
        assert_eq!(object["subtype"], json!("node"));
        assert_eq!(object["className"], json!("UILabel"));
        assert!(object.get("objectId").is_some() || object.get("preview").is_some());
    }

    #[test]
    fn resolve_node_errors_on_unknown_id() {
        let domain = DomDomain::new();
        let reply = domain.resolve_node(&request("DOM.resolveNode", json!({ "nodeId": 12345 })));
        assert!(matches!(reply, CdpReply::Error { .. }));
    }
}
