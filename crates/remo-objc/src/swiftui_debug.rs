//! Best-effort SwiftUI subtree extraction, spliced under a `UIHostingView`
//! (etc.) node so the Elements panel shows real SwiftUI view names
//! (`SwiftUI.VStack`, `SwiftUI.Text`, ...) instead of stopping at the
//! mangled hosting-view class name.
//!
//! # This is fundamentally different from the rest of `remo-objc`
//!
//! Every other module here (`user_defaults`, `filesystem`, `view_tree`
//! itself) only calls public, documented UIKit/Foundation API. This module
//! is the one exception: it calls a private, undocumented Objective-C
//! selector (`makeViewDebugData` and two older fallback names) that SwiftUI
//! only populates when the private `SWIFTUI_VIEW_DEBUG` environment
//! variable was set *before the first SwiftUI view built* (normally: before
//! `UIApplicationMain`/`@main App` starts, e.g. via the Xcode scheme's
//! launch environment — this crate does not, and should not, try to flip
//! that env var itself at runtime, since by the time any Remo code runs the
//! first SwiftUI views have almost always already built). The payload
//! format (`NSData` containing either JSON on newer OS versions or a
//! property list on older ones) and the meaning of each node's `flags`
//! field are themselves undocumented and reverse-engineered — see the
//! module-level doc comment on [`crate::view_tree`] for where this splices
//! in, and treat every detail here as liable to silently change or vanish
//! on a future OS release.
//!
//! ## Explicit opt-in, not silently-on
//!
//! Every entry point below is a no-op (returns `None`, changing nothing
//! about the existing plain-UIKit walk) unless the `SWIFTUI_VIEW_DEBUG`
//! environment variable is actually set in the process — which nothing in
//! Remo sets on the app's behalf. A caller who wants this must set it
//! themselves (e.g. in their Xcode scheme's launch environment, as the
//! bundled `RemoExample` demo app's shared scheme does, specifically to
//! demonstrate this capability). This is the opt-in mechanism: no separate
//! CDP-facing capability flag exists because one would arrive too late to
//! matter (SwiftUI must already be recording by the time the first hosting
//! view builds its content).
//!
//! ## Defensive-by-construction
//!
//! Every private-API call site below checks `respondsToSelector:` first and
//! falls back to "produce nothing, let the caller keep showing the opaque
//! hosting view" on any failure: selector missing, `NSData` empty, JSON
//! parse failure, or a shape that doesn't look like what we expect. Nothing
//! here should ever panic or crash the host app — a private API disappearing
//! must only ever cost us the SwiftUI detail, never the rest of the Elements
//! panel.

// The payload-decoding functions below are only ever called from
// `apple::dump_hosting_view`, which is gated on `feature = "uikit"` — but
// they're written as plain, portable functions (not nested inside that
// `cfg` block) specifically so `cargo test -p remo-objc` exercises the
// actual decoding logic on any host, uikit feature or not. That leaves them
// legitimately unreferenced in a non-uikit *non-test* build; silence dead
// code there rather than gating the functions themselves behind `uikit` and
// losing that portable test coverage.
#![cfg_attr(not(feature = "uikit"), allow(dead_code))]

use crate::view_tree::{Frame, ViewNode};

/// Class-name substrings that mark a `UIView` as a SwiftUI hosting seam
/// worth trying to expand. Matches the walk already done in
/// `view_tree.rs`'s `walk_view` — this module just adds an extra step when
/// one of these is seen, it does not change how the walk itself finds
/// views.
pub const HOSTING_VIEW_MARKERS: &[&str] = &[
    "HostingView",
    "HostingScrollView",
    "PlatformViewHost",
    "PlatformGroupContainer",
];

/// Whether `class_name` looks like a SwiftUI hosting seam.
pub fn is_hosting_view_class(class_name: &str) -> bool {
    HOSTING_VIEW_MARKERS
        .iter()
        .any(|marker| class_name.contains(marker))
}

/// Whether the private recorder was actually turned on for this process.
/// Nothing in Remo sets this env var itself — see the module doc for why.
pub fn recording_enabled() -> bool {
    std::env::var_os("SWIFTUI_VIEW_DEBUG").is_some()
}

// ---------------------------------------------------------------------------
// Apple target implementation
// ---------------------------------------------------------------------------

#[cfg(all(target_vendor = "apple", feature = "uikit"))]
mod apple {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{msg_send, sel};
    use objc2_foundation::NSData;
    use objc2_ui_kit::UIView;

    /// Whether `obj` is an instance of `class_name` (or a subclass) —
    /// checked at the actual runtime class, not assumed from the selector
    /// name. Every private-API return value gets this check before it is
    /// cast to a concrete typed binding, precisely so a future OS version
    /// returning `nil`, a different class, or nothing recognizable at all
    /// degrades to "show the opaque hosting view" instead of a type-encoding
    /// mismatch panic. Mirrors `user_defaults.rs`'s `is_kind_of` helper.
    unsafe fn is_kind_of(obj: *mut AnyObject, class_name: &std::ffi::CStr) -> bool {
        if obj.is_null() {
            return false;
        }
        let Some(class) = objc2::runtime::AnyClass::get(class_name) else {
            return false;
        };
        msg_send![obj, isKindOfClass: class]
    }

    /// Try each known selector name, in the order the write-up lists them
    /// (newest/most-official-feeling first), returning the raw `NSData`
    /// payload from whichever one the view actually responds to.
    ///
    /// # Safety
    /// `view` must be a valid, live `UIView`.
    unsafe fn call_debug_data_selector(view: &UIView) -> Option<*mut AnyObject> {
        let obj: &AnyObject = view;
        for selector in [
            sel!(makeViewDebugData),
            sel!(_viewDebugData),
            sel!(viewDebugData),
        ] {
            let responds: bool = msg_send![obj, respondsToSelector: selector];
            if !responds {
                continue;
            }
            let data: *mut AnyObject = msg_send![obj, performSelector: selector];
            if !data.is_null() {
                return Some(data);
            }
        }
        None
    }

    /// Copies an `NSData*` into an owned `Vec<u8>`.
    ///
    /// Goes through `objc2_foundation::NSData`'s own typed binding rather
    /// than hand-rolled `msg_send![data, bytes]` — found the hard way
    /// during the initial spike: the debug-data payload comes back as a
    /// `__NSSwiftData` (a Swift-native `Data` bridged to `NSData`), and
    /// objc2's message-send type-encoding validation panicked
    /// (`"invalid message send to -[Foundation.__NSSwiftData bytes]:
    /// expected return to have type code '^v', but found '*'"`) on a
    /// manual `*const u8`-typed `bytes` call against that specific
    /// bridged subclass. `NSData::as_bytes_unchecked` sidesteps that by
    /// using the crate's own correctly-encoded binding instead of a
    /// hand-guessed one.
    ///
    /// # Safety
    /// `data` must be a valid `NSData*` (or null).
    unsafe fn nsdata_to_vec(data: *mut AnyObject) -> Option<Vec<u8>> {
        if data.is_null() {
            return None;
        }
        // Verify the actual runtime class before casting to the typed
        // `NSData` binding — the selector name promises `NSData`, but
        // nothing stops a future OS from returning something else (or the
        // private selector name being repurposed entirely). Bridged Swift
        // `Data` values (`__NSSwiftData`, seen in practice on iOS 26) are
        // still real `NSData` instances per `isKindOfClass:`, so this
        // check accepts them without needing to special-case the bridged
        // class name.
        if !is_kind_of(data, c"NSData") {
            return None;
        }
        let ns_data: &NSData = &*(data as *const NSData);
        let bytes = ns_data.as_bytes_unchecked();
        if bytes.is_empty() {
            return None;
        }
        Some(bytes.to_vec())
    }

    /// Spike-only diagnostic: writes the raw payload to
    /// `$SWIFTUI_DEBUG_DUMP_DIR/<class>.json` for offline inspection.
    /// Gated behind an explicit second env var so it never fires outside a
    /// deliberate investigation session; not part of the shipped behavior
    /// this module offers a CDP client.
    fn dump_raw_payload_for_spike(class_name: &str, bytes: &[u8]) {
        let Some(dir) = std::env::var_os("SWIFTUI_DEBUG_DUMP_DIR") else {
            return;
        };
        let safe_name: String = class_name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = std::path::Path::new(&dir).join(format!("{safe_name}.json"));
        if let Err(e) = std::fs::write(&path, bytes) {
            tracing::debug!(error = %e, path = %path.display(), "spike dump write failed");
        }
    }

    /// The actual private-API round trip for one hosting view: call the
    /// debug-data selector, copy the payload out, and decode it into
    /// `ViewNode` children ready to splice under the hosting view's own
    /// node. Returns `None` on any *recognized* failure at any step (missing
    /// selector, empty/wrong-class payload, unparseable JSON) — but see
    /// `dump_hosting_view` below for the other, unrecognized failure mode
    /// this can't protect against on its own.
    ///
    /// # Safety
    /// `view` must be a valid, live `UIView` on the main thread (matches
    /// every other UIKit-touching function in this crate).
    unsafe fn dump_hosting_view_inner(view: &UIView, class_name: &str) -> Option<Vec<ViewNode>> {
        let data = call_debug_data_selector(view)?;
        // Retain the NSData we were handed (it's a `+0` return per ObjC
        // convention for these accessor-shaped selectors, but treat it as
        // borrowed and explicitly retain/release around the copy rather
        // than assume) so the copy below can't race a deallocation.
        let retained: Option<Retained<AnyObject>> = Retained::retain(data);
        let bytes = retained
            .as_deref()
            .and_then(|d| nsdata_to_vec(d as *const _ as *mut _))?;
        if std::env::var_os("SWIFTUI_DEBUG_DUMP_DIR").is_some() {
            dump_raw_payload_for_spike(class_name, &bytes);
        }
        match decode_payload(&bytes) {
            Some(nodes) => Some(nodes),
            None => {
                tracing::debug!(
                    class = %class_name,
                    bytes = bytes.len(),
                    "SwiftUI debug payload did not parse as JSON or plist; showing hosting view opaquely"
                );
                None
            }
        }
    }

    /// Attempts the full private-API round trip for one hosting view.
    /// Returns an empty `Vec` (never panics) on *any* failure, so the
    /// caller's fallback is always just "show the opaque hosting view, like
    /// before this module existed."
    ///
    /// The inner logic (`dump_hosting_view_inner`) already handles every
    /// failure mode it can *recognize* (missing selector, wrong class,
    /// unparseable payload) by returning `None`. This wrapper additionally
    /// catches the failure mode it *can't* recognize in advance: objc2's own
    /// runtime type-encoding verification panicking when a private,
    /// undocumented selector's actual Objective-C method signature doesn't
    /// match what a `msg_send!` call assumed. This was caught for real
    /// during this feature's initial spike — a bridged Swift `Data` object
    /// (`__NSSwiftData`) returned by `makeViewDebugData` on iOS 26 turned
    /// out to have a non-standard `-bytes` method encoding, and objc2
    /// panicked rather than risk undefined behavior. `is_kind_of`/typed
    /// bindings close that specific hole, but this is reverse-engineered,
    /// undocumented private API surface — a *different* selector or a
    /// future OS version can hit the same class of mismatch in a way no
    /// amount of `respondsToSelector:`/class checking can predict in
    /// advance. `catch_unwind` is the general-purpose safety net for
    /// exactly that: it's caught here, still well inside Rust-called code
    /// (this function's own caller, `walk_view`), not across the
    /// `run_on_main_sync` GCD trampoline's `extern "C"` boundary further up
    /// the stack — panicking *across* that boundary is what previously
    /// produced "failed to initiate panic... aborting" instead of a
    /// catchable unwind.
    ///
    /// # Safety
    /// `view` must be a valid, live `UIView` on the main thread (matches
    /// every other UIKit-touching function in this crate).
    pub unsafe fn dump_hosting_view(view: &UIView) -> Vec<ViewNode> {
        if !recording_enabled() {
            return Vec::new();
        }
        let class_name = view.class().name().to_str().unwrap_or("").to_owned();
        if !is_hosting_view_class(&class_name) {
            return Vec::new();
        }
        let view_ptr: *const UIView = view;
        let class_name_for_panic = class_name.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            // SAFETY: `view_ptr` was derived from the `&UIView` this
            // function was called with, which the caller guarantees is
            // valid and main-thread-confined for the duration of this call.
            dump_hosting_view_inner(&*view_ptr, &class_name_for_panic)
        }));
        match result {
            Ok(Some(nodes)) => nodes,
            Ok(None) => {
                tracing::debug!(
                    class = %class_name,
                    "SwiftUI debug-data selector unavailable, empty, or unparseable; showing hosting view opaquely"
                );
                Vec::new()
            }
            Err(_) => {
                tracing::warn!(
                    class = %class_name,
                    "SwiftUI debug-data extraction panicked (likely an objc2 \
                     type-encoding mismatch against a private, undocumented \
                     selector on this OS version); falling back to the \
                     opaque hosting view instead of crashing"
                );
                Vec::new()
            }
        }
    }
}

#[cfg(not(all(target_vendor = "apple", feature = "uikit")))]
mod apple {}

// ---------------------------------------------------------------------------
// Payload decoding (portable — exercised by `cargo test` on any host)
// ---------------------------------------------------------------------------

/// Node `flags` values, per the reverse-engineered write-up this module is
/// based on. Undocumented, may change across OS versions. `0`
/// (geometry/environment/misc) has no named constant since every use of it
/// below is the catch-all `_` arm rather than an explicit match — see that
/// arm's comment.
mod flags {
    pub const VIEW_TYPE: i64 = 1;
    pub const MODIFIER: i64 = 2;
}

/// Decode a `makeViewDebugData` payload (JSON on iOS 26+, property list on
/// older OS versions) into `ViewNode`s ready to splice under the hosting
/// view. Returns `None` on anything unrecognized rather than guessing.
fn decode_payload(bytes: &[u8]) -> Option<Vec<ViewNode>> {
    match parse_json_deeply_nested(bytes) {
        Ok(json) => return decode_json_nodes(&json),
        Err(e) => {
            tracing::debug!(bytes = bytes.len(), error = %e, "SwiftUI debug payload JSON parse error");
        }
    }
    // Older OS versions return a binary/XML property list instead of JSON.
    // This crate has no plist dependency today and the write-up's own
    // confirmed sample was JSON (iOS 26), so rather than add a whole extra
    // parser for a format we cannot currently verify against, this is an
    // explicit, documented gap: fall back to "nothing to splice", which
    // callers already treat as "just show the opaque hosting view."
    let head_len = bytes.len().min(64);
    tracing::debug!(
        bytes = bytes.len(),
        head_hex = %bytes[..head_len].iter().map(|b| format!("{b:02x}")).collect::<String>(),
        head_ascii = %String::from_utf8_lossy(&bytes[..head_len]),
        "SwiftUI debug payload is not JSON (likely a pre-iOS-26 property list); \
         plist decoding isn't implemented, falling back to the opaque hosting view"
    );
    None
}

/// Parses JSON that may nest well past serde_json's default 128-level
/// recursion limit — confirmed necessary empirically: real
/// `makeViewDebugData` payloads from the demo app on iOS 26.2 hit
/// "recursion limit exceeded" a few tens of KB into a few-hundred-KB blob.
/// `disable_recursion_limit()` alone would trade that error for a real
/// stack overflow on a deep enough tree; `serde_stacker::maybe_grow` grows
/// the actual OS thread stack as needed so deep-but-legitimate input
/// parses instead of either failing or crashing.
fn parse_json_deeply_nested(bytes: &[u8]) -> serde_json::Result<serde_json::Value> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    deserializer.disable_recursion_limit();
    let deserializer = serde_stacker::Deserializer::new(&mut deserializer);
    serde::de::Deserialize::deserialize(deserializer)
}

fn decode_json_nodes(value: &serde_json::Value) -> Option<Vec<ViewNode>> {
    let array = value.as_array()?;
    let nodes: Vec<ViewNode> = array.iter().filter_map(decode_json_node).collect();
    if nodes.is_empty() {
        None
    } else {
        Some(nodes)
    }
}

/// Decodes one `{"properties": [{"id": N, "attribute": {"type", "flags",
/// "readableType"?, "value"?, "subattributes"?}}, ...], "children": [...]}`
/// node — this exact shape (a property's `flags`/`type`/`readableType`
/// nested *under* an `"attribute"` object, not flat on the property itself)
/// was confirmed empirically against real payloads captured from the demo
/// app on iOS 26.2 (`SWIFTUI_DEBUG_DUMP_DIR`, see `dump_raw_payload_for_spike`
/// above); it is not from the original write-up's — necessarily incomplete —
/// description alone.
///
/// Returns `None` only if there is truly nothing to show — no `flags:1`
/// view-type property *and* no `flags:2` modifier property either. A node
/// with a type name is shown with whatever modifier detail was found, and a
/// node with only modifiers (confirmed to occur in real payloads — a bare
/// wrapper like `NavigationSearchAdjustmentModifier` with no view type of
/// its own) falls back to a synthetic `<modifier: ...>` label rather than
/// being dropped, since dropping it would silently swallow its entire
/// subtree too (see `decode_json_nodes`'s `filter_map`).
fn decode_json_node(value: &serde_json::Value) -> Option<ViewNode> {
    let properties = value.get("properties")?.as_array()?;

    let mut type_name: Option<String> = None;
    let mut modifiers: Vec<String> = Vec::new();

    for prop in properties {
        let Some(attribute) = prop.get("attribute") else {
            continue;
        };
        let Some(flag) = attribute.get("flags").and_then(serde_json::Value::as_i64) else {
            continue;
        };
        // `readableType` is the short, human-friendly form SwiftUI itself
        // generates (e.g. `"VStack<Text>"`); `type` is the fully-qualified
        // mangled-ish form (`"SwiftUI.VStack<SwiftUI.Text>"`). Prefer the
        // former — it's what the write-up's examples show — falling back to
        // the latter for any attribute that only has one of the two.
        let text = attribute_text(attribute);
        match flag {
            flags::VIEW_TYPE => {
                if let Some(t) = text {
                    type_name = Some(t);
                }
            }
            flags::MODIFIER => {
                // "Modifier payloads are often null" under the safe env-var
                // path — fall back to a bare `<unknown modifier>` rather
                // than skipping the entry silently, so the count of
                // modifiers applied is still visible even when their detail
                // isn't.
                modifiers.push(text.unwrap_or_else(|| "<unknown modifier>".to_string()));
            }
            _ => {
                // flags:0 covers geometry/environment/misc attributes
                // (`__C.CGSize`, `EnvironmentValues`, `LayoutComputer`, ...).
                // No confirmed, stable field name for frame geometry was
                // found in the payloads captured during this spike (unlike
                // the write-up's claim that frames "come straight from the
                // blob") — rather than guess at a shape that might silently
                // misread unrelated data as a frame, this is left as a
                // known gap: spliced SwiftUI nodes report a zero frame, and
                // `DOM.getBoxModel` on them falls back accordingly (same
                // honest-gap treatment `domain_dom.rs` already documents for
                // window-coordinate conversion).
            }
        }
    }

    // Real captured payloads include nodes that carry only modifier
    // (flags:2) properties and no flags:1 view-type entry of their own —
    // e.g. a bare `NavigationSearchAdjustmentModifier` wrapper. Dropping
    // those entirely (as an earlier version of this function did) silently
    // swallowed their entire subtree, since `decode_json_nodes` filters out
    // anything this function returns `None` for. Falling back to a
    // synthetic `<modifier: ...>` label keeps the subtree intact; only a
    // node with neither a type name nor any modifiers is truly nothing to
    // show, and gets dropped.
    let class_name = match (type_name, modifiers.is_empty()) {
        (Some(t), true) => t,
        (Some(t), false) => format!("{t} [{}]", modifiers.join(", ")),
        (None, false) => format!("<modifier: {}>", modifiers.join(", ")),
        (None, true) => return None,
    };

    let children = value
        .get("children")
        .and_then(serde_json::Value::as_array)
        .map(|kids| kids.iter().filter_map(decode_json_node).collect())
        .unwrap_or_default();

    Some(ViewNode {
        class_name,
        frame: Frame {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        },
        is_hidden: false,
        alpha: 1.0,
        tag: 0,
        accessibility_id: None,
        children,
    })
}

/// Prefers `attribute.readableType` (short, human-facing) over
/// `attribute.type` (fully-qualified) over `attribute.value` (seen on
/// `subattributes`-style leaf entries, e.g. `"searchAdjustment": "disabled"`).
fn attribute_text(attribute: &serde_json::Value) -> Option<String> {
    for key in ["readableType", "type", "value"] {
        if let Some(s) = attribute.get(key).and_then(serde_json::Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

#[cfg(all(target_vendor = "apple", feature = "uikit"))]
pub use apple::dump_hosting_view;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_disabled_by_default() {
        std::env::remove_var("SWIFTUI_VIEW_DEBUG");
        assert!(!recording_enabled());
    }

    #[test]
    fn hosting_view_markers_match_known_class_names() {
        assert!(is_hosting_view_class(
            "_TtCC7SwiftUI20TabHostingControllerP33_E387C3C47C0D2A0931533D8490A5A8B711HostingView"
        ));
        assert!(is_hosting_view_class(
            "_TtGC7SwiftUI14_UIHostingViewGVS_15ModifiedContentVS_7AnyViewVS_12RootModifier__"
        ));
        assert!(!is_hosting_view_class("UILabel"));
    }

    #[test]
    fn decodes_a_minimal_json_node() {
        // Shape confirmed empirically against real `makeViewDebugData`
        // payloads captured from the demo app (iOS 26.2 simulator): each
        // property nests its `flags`/`type`/`readableType` under an
        // `"attribute"` object, keyed by a separate `"id"`.
        let json = serde_json::json!([{
            "properties": [
                { "id": 0, "attribute": { "flags": 1, "type": "SwiftUI.VStack<SwiftUI.TupleView<(SwiftUI.Text, SwiftUI.Text)>>", "readableType": "VStack<TupleView<(Text, Text)>>" } },
                { "id": 1, "attribute": { "flags": 2, "type": "SwiftUI._PaddingLayout", "readableType": "_PaddingLayout" } }
            ],
            "children": [
                {
                    "properties": [
                        { "id": 0, "attribute": { "flags": 1, "type": "SwiftUI.Text", "readableType": "Text" } }
                    ],
                    "children": []
                }
            ]
        }]);
        let nodes = decode_json_nodes(&json).expect("should decode");
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].class_name.contains("VStack"));
        assert!(nodes[0].class_name.contains("_PaddingLayout"));
        assert_eq!(nodes[0].children.len(), 1);
        assert_eq!(nodes[0].children[0].class_name, "Text");
    }

    #[test]
    fn null_modifier_falls_back_to_placeholder_instead_of_dropping() {
        let json = serde_json::json!([{
            "properties": [
                { "id": 0, "attribute": { "flags": 1, "type": "SwiftUI.Text", "readableType": "Text" } },
                { "id": 1, "attribute": { "flags": 2 } }
            ],
            "children": []
        }]);
        let nodes = decode_json_nodes(&json).expect("should decode");
        assert!(nodes[0].class_name.contains("<unknown modifier>"));
    }

    #[test]
    fn node_with_only_a_modifier_falls_back_to_a_synthetic_label_not_dropped() {
        // Confirmed to occur in real captured payloads: a node whose only
        // property is a modifier (flags:2), with no flags:1 view type of
        // its own. Must not be dropped — that would silently swallow
        // whatever subtree hangs under it.
        let json = serde_json::json!([{
            "properties": [
                { "id": 0, "attribute": { "flags": 2, "type": "SwiftUI._PaddingLayout", "readableType": "_PaddingLayout" } }
            ],
            "children": []
        }]);
        let nodes = decode_json_nodes(&json).expect("should decode, not drop");
        assert!(nodes[0].class_name.contains("_PaddingLayout"));
    }

    #[test]
    fn node_with_no_type_and_no_modifier_is_dropped_not_shown_blank() {
        let json = serde_json::json!([{
            "properties": [],
            "children": []
        }]);
        assert!(decode_json_nodes(&json).is_none());
    }

    #[test]
    fn garbage_bytes_decode_to_none_not_a_panic() {
        assert!(decode_payload(b"not json and not a plist").is_none());
    }

    #[test]
    fn empty_bytes_decode_to_none() {
        assert!(decode_payload(b"").is_none());
    }

    /// A real (trimmed) fragment of an actual `makeViewDebugData` payload,
    /// captured from the `RemoExample` demo app on an iOS 26.2 simulator
    /// during this feature's spike — not a hand-written guess at the shape.
    /// Exercises `subattributes` (an extra nesting level found only on some
    /// flags:0 attributes) and confirms it's ignored gracefully rather than
    /// mis-parsed.
    #[test]
    fn decodes_a_real_captured_fragment_with_subattributes() {
        let json = serde_json::json!([{
            "properties": [
                {
                    "id": 1,
                    "attribute": {
                        "readableType": "ModifiedContent<_ConditionalContent<_ViewList_View, TabItemGroup.HostView>, NavigationSearchAdjustmentModifier>",
                        "type": "SwiftUI.ModifiedContent<SwiftUI._ConditionalContent<SwiftUI._ViewList_View, SwiftUI.TabItemGroup.HostView>, SwiftUI.NavigationSearchAdjustmentModifier>",
                        "flags": 0
                    }
                },
                {
                    "id": 0,
                    "attribute": {
                        "readableType": "ModifiedContent<_ConditionalContent<_ViewList_View, TabItemGroup.HostView>, NavigationSearchAdjustmentModifier>",
                        "flags": 1,
                        "type": "SwiftUI.ModifiedContent<SwiftUI._ConditionalContent<SwiftUI._ViewList_View, SwiftUI.TabItemGroup.HostView>, SwiftUI.NavigationSearchAdjustmentModifier>"
                    }
                }
            ],
            "children": [
                {
                    "properties": [
                        {
                            "attribute": {
                                "type": "SwiftUI.NavigationSearchAdjustmentModifier",
                                "readableType": "NavigationSearchAdjustmentModifier",
                                "flags": 2
                            },
                            "id": 0
                        },
                        {
                            "attribute": {
                                "flags": 0,
                                "type": "SwiftUI.NavigationSearchAdjustmentModifier",
                                "readableType": "NavigationSearchAdjustmentModifier",
                                "subattributes": [
                                    {
                                        "name": "searchAdjustment",
                                        "readableType": "SearchAdjustment",
                                        "value": "disabled",
                                        "type": "SwiftUI.SearchAdjustment",
                                        "flags": 0
                                    }
                                ]
                            },
                            "id": 1
                        }
                    ],
                    "children": []
                }
            ]
        }]);
        let nodes = decode_json_nodes(&json).expect("should decode");
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0]
            .class_name
            .contains("NavigationSearchAdjustmentModifier"));
        assert_eq!(nodes[0].children.len(), 1);
        assert!(nodes[0].children[0]
            .class_name
            .contains("NavigationSearchAdjustmentModifier"));
    }
}
