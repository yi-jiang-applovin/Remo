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
//! first SwiftUI views have almost always already built).
//!
//! **Gotcha confirmed the hard way**: a scheme's `EnvironmentVariables`
//! block (what `RemoExample.xcscheme` sets) only takes effect when Xcode
//! itself launches the app (its own Run action, or `xcodebuild ... build
//! test`-style actions that go through the scheme). Driving the simulator
//! through other tooling — e.g. `xcodebuildmcp simulator build-and-run`,
//! which installs the build product and launches it via `simctl` directly —
//! does **not** read the scheme's env vars at all, silently. To activate
//! this capability from that kind of tooling, launch (or relaunch) the app
//! with the `SIMCTL_CHILD_` env-var prefix `simctl launch` itself supports,
//! e.g.:
//! `SIMCTL_CHILD_SWIFTUI_VIEW_DEBUG=1 xcrun simctl launch <udid> com.remo.example`.
//! The payload
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
// OS version allowlist
// ---------------------------------------------------------------------------
//
// Deliberately an *allowlist*, not a "try it and see if it parses" — for a
// pipeline built entirely on reverse-engineered private API, "the payload
// decoded without an error" is not evidence it decoded *correctly*. The
// dangerous failure mode here isn't a hard parse failure (that's visible:
// an empty subtree, a debug log). It's the OS changing `flags` semantics or
// the payload shape *just* enough that decoding still succeeds but produces
// a plausible-looking, semantically wrong tree — silently misleading data
// in a debugging tool, which is worse than the tool visibly not working.
// Getting this pipeline working at all took two real crash/correctness
// fixes discovered only by running it for real (an objc2 type-encoding
// panic on `-bytes`, a serde_json recursion-limit failure, and the true
// `attribute`-nested JSON schema — none of that was predictable from the
// original write-up alone). That history is exactly why "parses" isn't
// trusted as "verified" here: every OS major version below must have
// actually been run through the Phase-0 spike (dump the raw payload via
// `SWIFTUI_DEBUG_DUMP_DIR`, confirm it decodes into a tree that matches the
// real view hierarchy) before being added.

/// OS major versions this module's private-API pipeline has actually been
/// spike-verified against. Confirmed specifically on iOS 26.2 (see this
/// module's git history); iOS 26.x in general is assumed close enough to
/// share the same payload shape, but no other major version has been
/// checked at all.
const VERIFIED_MAJOR_OS_VERSIONS: &[i64] = &[26];

/// Portable so it's unit-testable without objc2/a real device — see
/// `current_os_version` (Apple-only, below) for where the actual version
/// numbers come from.
fn is_verified_os_version(major: i64) -> bool {
    VERIFIED_MAJOR_OS_VERSIONS.contains(&major)
}

/// Logged at most once per process — a deep view hierarchy can contain many
/// hosting views, and every one of them would otherwise repeat this warning
/// for a single `DOM.getDocument` call.
static WARNED_UNSUPPORTED_OS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn warn_unsupported_os_once(major: i64, minor: i64, patch: i64) {
    if WARNED_UNSUPPORTED_OS.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    tracing::warn!(
        os_version = %format!("{major}.{minor}.{patch}"),
        "unsupported OS {major}.{minor}.{patch}, skipping SwiftUI debug capture — \
         has this been verified with the Phase-0 spike test? see swiftui_debug.rs"
    );
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
    use objc2_foundation::{NSData, NSProcessInfo};
    use objc2_ui_kit::UIView;

    /// The running process's `(major, minor, patch)` OS version, via the
    /// fully public `NSProcessInfo.operatingSystemVersion` — the one piece
    /// of this module that talks to a documented API, specifically so the
    /// version gate itself can't be the thing that breaks.
    fn current_os_version() -> (i64, i64, i64) {
        let info = NSProcessInfo::processInfo();
        let v = info.operatingSystemVersion();
        (
            v.majorVersion as i64,
            v.minorVersion as i64,
            v.patchVersion as i64,
        )
    }

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
        // Version gate: short-circuit before any private-API call is even
        // attempted (not just before decoding) on an OS major version this
        // pipeline hasn't been spike-verified against. See the module doc
        // above `VERIFIED_MAJOR_OS_VERSIONS` for why "it might just parse
        // fine anyway" is exactly the risk this refuses to take.
        let (major, minor, patch) = current_os_version();
        if !is_verified_os_version(major) {
            warn_unsupported_os_once(major, minor, patch);
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
    reset_filter_counters();
    let array = value.as_array()?;
    let nodes: Vec<ViewNode> = array.iter().filter_map(decode_json_node).collect();
    log_filter_summary();
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
    //
    // The view's own type name and its modifier list are kept as two
    // separate fields (`class_name` / `modifiers`) rather than one
    // concatenated string — SwiftUI's modifier chains are already deeply
    // nested generics on their own, and appending them to `class_name`
    // (which `domain_dom.rs` maps straight to the CDP `nodeName` the
    // Elements panel displays as the tag name) produced unreadable
    // multi-hundred-character tags. `domain_dom.rs`'s `attributes()`
    // surfaces `modifiers` as its own CDP attribute instead.
    //
    // That alone isn't enough, though: a `flags:1` type string can *itself*
    // be a `ModifiedContent<Base, Modifier>` — SwiftUI's own type system
    // nests modifiers into the type, not just this payload's separate
    // `flags:2` properties. Confirmed against a real captured node in this
    // demo app: a 3018-character `readableType` that was
    // `ModifiedContent<ModifiedContent<NavigationStackStyledCore<...>, ...>,
    // ...>` — no sibling `flags:2` property to split off at all, so the
    // step-1 fix alone left it untouched. `unwrap_modified_content` peels
    // that apart the same way, recursively, before it ever becomes
    // `class_name`.
    let class_name = match &type_name {
        Some(t) => {
            let (base, peeled) = unwrap_modified_content(t);
            modifiers.extend(peeled);
            base
        }
        // The "only a modifier, no view type" case still needs *some*
        // label — synthesize one from the modifier list rather than an
        // empty tag name (and don't also duplicate it into `modifiers`,
        // since it's standing in for the missing type name here, not
        // describing a modifier applied *to* a named view).
        None if !modifiers.is_empty() => format!("<modifier: {}>", modifiers.join(", ")),
        None => return None,
    };
    // Only nodes that had a real view type of their own carry a separate
    // `modifiers` list; the synthetic `<modifier: ...>` label above already
    // encodes that same information in `class_name` for the type-less case.
    let modifiers = if type_name.is_some() {
        modifiers
    } else {
        Vec::new()
    };
    // Defensive cap, independent of the `ModifiedContent`-unwrapping above:
    // the *base* type left after unwrapping can still be enormous on its
    // own (that same real 3018-char node's base, after peeling 2 modifiers,
    // was `NavigationStackStyledCore<...>` wrapping a whole `VStack` tree —
    // still thousands of characters). This isn't
    // `LKS_SwiftUICompactFilter`-grade structural recognition for every
    // container shape (out of scope for this pass); it's a hard length
    // limit so nothing ever reaches a tag name in the thousands of
    // characters again, full stop.
    let class_name = cap_display_length(&class_name, MAX_CLASS_NAME_CHARS);

    // Bounded properties flattener (independent of the tag-name logic
    // above) — walks this node's full attribute tree, including nested
    // `subattributes`, into flat rows for `domain_dom.rs`'s
    // `CSS.getComputedStyleForNode` pseudo-CSS mapping. See
    // `flatten_node_properties`'s doc comment for the depth/row limits and
    // truncation behavior.
    let style_rows = flatten_node_properties(properties, FLATTEN_DEPTH_LIMIT, FLATTEN_ROW_CAP);

    let raw_children: Vec<ViewNode> = value
        .get("children")
        .and_then(serde_json::Value::as_array)
        .map(|kids| kids.iter().filter_map(decode_json_node).collect())
        .unwrap_or_default();
    // Node-hiding filter (see `classify`'s doc comment) — applied here,
    // one level up from where each child was itself decoded, so an
    // `ElideHoist` decision can splice that child's own (already
    // bottom-up-filtered) children directly into *this* node's child list.
    let children = apply_view_filter(raw_children);

    count_decoded_node();

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
        modifiers,
        style_rows,
        children,
    })
}

// ---------------------------------------------------------------------------
// Part A — recursive `ModifiedContent<Base, Modifier>` unwrapping for the
// tag name, and a hard length cap as the last line of defense.
// ---------------------------------------------------------------------------

/// Hard cap on `class_name`'s displayed length, applied after
/// `ModifiedContent` unwrapping. Not a structural-recognition attempt at
/// every possible huge container shape (`TupleView<(A, B, C, ...)>` and
/// friends can still be long) — just a safety net so nothing this deep ever
/// produces a tag name in the thousands of characters again. Starting
/// point, not a carefully-tuned exact value; adjust if real payloads
/// suggest a different number reads better in practice.
const MAX_CLASS_NAME_CHARS: usize = 200;

/// Truncates `s` to at most `max_chars` *characters* (not bytes — these
/// strings are printable ASCII in every payload seen so far, but char-safe
/// truncation costs nothing and avoids ever panicking on a multi-byte
/// boundary), appending `…` when truncation actually happened.
fn cap_display_length(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut truncated: String = s.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

/// Recursively peels `ModifiedContent<Base, Modifier>` off a SwiftUI type
/// string, the way SwiftUI's own type system nests a view's applied
/// modifiers *into its type* — a `flags:2` property is a separate, sibling
/// way modifiers show up in this payload, but this is the other one, and
/// nothing about the payload's `properties` array signals it; it only shows
/// up by pattern-matching the type string itself.
///
/// Returns `(base_type, peeled_modifiers)` where `peeled_modifiers` is
/// ordered outermost-first (the modifier applied last, textually the
/// outermost wrapper, comes first) — an arbitrary but documented choice;
/// nothing currently depends on the order.
///
/// Stops as soon as the outermost shape no longer parses as
/// `ModifiedContent<X, Y>` (including: wrong number of top-level generic
/// arguments, which a real `ModifiedContent` should never have but a
/// different, unanticipated shape might) — never guesses past what it can
/// actually parse.
fn unwrap_modified_content(type_str: &str) -> (String, Vec<String>) {
    let mut current = type_str.trim().to_string();
    let mut modifiers = Vec::new();
    while let Some((name, mut args)) = parse_outer_generic(&current) {
        if !is_modified_content(name) || args.len() != 2 {
            break;
        }
        // `args` is [Base, Modifier] in source order.
        let modifier = args.remove(1);
        let base = args.remove(0);
        modifiers.push(modifier.trim().to_string());
        current = base.trim().to_string();
    }
    (current, modifiers)
}

/// Whether `name` (the part of a type string before its first top-level
/// `<`) refers to `ModifiedContent`, tolerating a module-qualified form
/// (`SwiftUI.ModifiedContent`) — `readableType` strings seen so far never
/// carry the module prefix, but `type` (fully-qualified) always does, and
/// nothing guarantees which one this function is ever called with.
fn is_modified_content(name: &str) -> bool {
    name == "ModifiedContent" || name.ends_with(".ModifiedContent")
}

/// Parses `Name<Arg1, Arg2, ...>` into `(Name, [Arg1, Arg2, ...])`, splitting
/// only on *top-level* commas — i.e. not commas nested inside another
/// `<...>` generic or a `(...)` tuple type (SwiftUI type strings routinely
/// contain both, e.g. `TupleView<(Text, Image)>`). Returns `None` if `s`
/// doesn't have this shape at all, or if the generic argument list isn't
/// properly balanced (defensive against a future payload format this
/// wasn't written against — never panics on malformed input).
fn parse_outer_generic(s: &str) -> Option<(&str, Vec<&str>)> {
    let open = s.find('<')?;
    // The whole string must be exactly `Name<...>` — if there's anything
    // after the matching close bracket, this isn't a single bare generic
    // type (could be a tuple element list, a qualified member reference
    // like `Foo<Bar>.Baz`, etc.) and guessing which part to unwrap would
    // risk silently mangling something this wasn't verified against.
    let close = matching_close_angle_bracket(s, open)?;
    if close != s.len() - 1 {
        return None;
    }
    let name = &s[..open];
    if name.is_empty() {
        return None;
    }
    let inner = &s[open + 1..close];
    Some((name, split_top_level_commas(inner)))
}

/// Finds the index of the `>` that closes the `<` at byte index `open`,
/// tracking nesting depth across `<>`, `()`, and `[]` (SwiftUI type strings
/// use tuple parens; brackets aren't known to appear, but costs nothing to
/// track defensively). Returns `None` if the brackets never balance.
fn matching_close_angle_bracket(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    debug_assert_eq!(bytes.get(open), Some(&b'<'));
    let mut depth: i32 = 0;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits `s` on top-level commas only — depth-tracked across `<>`, `()`,
/// and `[]` the same way `matching_close_angle_bracket` is, so a comma
/// inside a nested generic or tuple never causes a false split.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut parts = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(s[start..].trim());
    parts
}

// ---------------------------------------------------------------------------
// Part B — bounded properties flattener, feeding the pseudo-CSS surface.
// ---------------------------------------------------------------------------

/// Starting points for the flattener's bounds, not carefully-tuned exact
/// requirements — matching LookInside's own `SwiftUIAttributeFlattener`
/// signature (`depthLimit`/`rowCap`), reverse-engineered from its binary
/// symbols, which takes these as *parameters* rather than fixed constants.
/// Kept as plain constants here since nothing today calls this with a
/// different value, but nothing structurally prevents it either.
const FLATTEN_DEPTH_LIMIT: usize = 8;
const FLATTEN_ROW_CAP: usize = 50;
/// Per-row title/value length cap — the same class of problem
/// `MAX_CLASS_NAME_CHARS` guards against (a deeply-nested-generic type
/// string used verbatim), just applied to properties-panel rows instead of
/// the tag name. Confirmed necessary live: without this, a `flags:1`
/// attribute with no `name_hint`, no scalar value, and no subattributes
/// produces a row whose title *and* value are both its own (potentially
/// thousands-of-characters) type string.
const FLATTEN_ROW_TEXT_CHAR_LIMIT: usize = 150;

/// Flattens every property on a node — its `flags:0`/`1`/`2` attributes and
/// their nested `subattributes`, recursively — into a flat list of
/// `(title, value)` rows suitable for a CDP pseudo-CSS declaration list.
/// Not exact Swift-call-syntax reconstruction (no attempt at rendering
/// `.font(.title3)`); LookInside's own `jsonValueToString`/`compactJSON`
/// symbols don't do that either, just readable stringification, so this
/// doesn't try to clear a bar the reference implementation itself doesn't
/// clear.
///
/// Bounded on two axes, matching `SwiftUIAttributeFlattener.flatten`'s
/// signature: `depth_limit` caps how far into nested `subattributes` this
/// recurses, `row_cap` caps the total number of rows produced across the
/// whole node. When either limit stops the walk from descending into or
/// including something, a trailing `("…", "N more (truncated)")` row is
/// appended so the cut-off is visible in the CDP reply rather than a
/// silent drop — matching the spirit (not the exact wording) of the
/// `" more (truncated)"` string confirmed via `strings` against the real
/// `LookInsideServer.xcframework` binary.
fn flatten_node_properties(
    properties: &[serde_json::Value],
    depth_limit: usize,
    row_cap: usize,
) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for prop in properties {
        if let Some(attribute) = prop.get("attribute") {
            flatten_attribute(
                attribute,
                None,
                0,
                depth_limit,
                row_cap,
                &mut rows,
                &mut skipped,
            );
        }
    }
    if skipped > 0 {
        rows.push(("…".to_string(), format!("{skipped} more (truncated)")));
    }
    rows
}

/// One step of the flattener's recursion. `name_hint` is the title to use
/// when the caller already knows it (a `subattributes` entry's own
/// `"name"`, e.g. `"searchAdjustment"`); falls back to the attribute's own
/// type name (`attribute_text`) when there is none (true for every
/// top-level `flags:0/1/2` property, which don't carry a `"name"`).
///
/// A node beyond `depth_limit`, or once `row_cap` rows have already been
/// produced, is counted in `skipped` and *not* recursed into further —
/// deliberately not attempting an exact remaining-row count (which would
/// mean walking the rest of the tree anyway, defeating the point of a
/// bound), just a visible "something was cut off here" signal.
#[allow(clippy::too_many_arguments)]
fn flatten_attribute(
    attribute: &serde_json::Value,
    name_hint: Option<&str>,
    depth: usize,
    depth_limit: usize,
    row_cap: usize,
    rows: &mut Vec<(String, String)>,
    skipped: &mut usize,
) {
    if depth > depth_limit || rows.len() >= row_cap {
        *skipped += 1;
        return;
    }
    // Same hard cap as `class_name` (Part A), and for the same reason: a
    // top-level `flags:1`/`flags:2` attribute's own type name is exactly
    // the kind of deeply-nested-generic string that can be thousands of
    // characters, and here it's used verbatim as both a row's *title* (with
    // no `name_hint` to fall back on) and, in the no-value/no-subattributes
    // branch below, its *value* too — a `--swiftui-<huge string>` custom
    // property would just be the same tag-name problem restated inside the
    // Styles pane instead of fixed. Unlike `class_name`, this is not run
    // through `unwrap_modified_content` first (these rows are meant to be
    // short, readable labels, not a second place asserting exact SwiftUI
    // type-parsing correctness) — the flat length cap alone is enough here.
    let title = cap_display_length(
        &name_hint
            .map(str::to_string)
            .or_else(|| attribute_text(attribute))
            .unwrap_or_else(|| "?".to_string()),
        FLATTEN_ROW_TEXT_CHAR_LIMIT,
    );

    let scalar_value = attribute
        .get("value")
        .filter(|v| !v.is_null())
        .map(stringify_attribute_value);
    let subattributes = attribute
        .get("subattributes")
        .and_then(serde_json::Value::as_array)
        .filter(|subs| !subs.is_empty());

    match (scalar_value, subattributes) {
        (Some(value), _) => rows.push((
            title,
            cap_display_length(&value, FLATTEN_ROW_TEXT_CHAR_LIMIT),
        )),
        (None, Some(subs)) => {
            for sub in subs {
                let sub_name = sub.get("name").and_then(serde_json::Value::as_str);
                flatten_attribute(
                    sub,
                    sub_name,
                    depth + 1,
                    depth_limit,
                    row_cap,
                    rows,
                    skipped,
                );
            }
        }
        // Nothing scalar and nothing to recurse into — still informative
        // to show that this property exists, using its own type as the
        // value (e.g. a modifier with a genuinely empty payload under the
        // safe env-var path, matching the "modifier payloads are often
        // null" caveat this module already documents elsewhere).
        (None, None) => {
            if let Some(t) = attribute_text(attribute) {
                rows.push((title, cap_display_length(&t, FLATTEN_ROW_TEXT_CHAR_LIMIT)));
            }
        }
    }
}

/// Stringifies a JSON value for display — matching LookInside's own
/// `jsonValueToString`/`compactJSON` bar (readable stringification), not
/// exact Swift-syntax reconstruction. Scalars render as their natural text
/// form; objects/arrays render as compact JSON.
fn stringify_attribute_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
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

// ---------------------------------------------------------------------------
// Node-hiding filter — collapses purely-structural SwiftUI plumbing nodes
// (never real user content) out of the tree.
// ---------------------------------------------------------------------------
//
// Mirrors the *shape* of `LKS_SwiftUIFilterDecision` from the real
// `LookInsideServer.xcframework` binary, confirmed via `nm -a | swift
// demangle`:
//
//   enum LKS_SwiftUIFilterDecision: Equatable {
//       case keep
//       case elideHoist     // remove this node; splice its children up
//       case elideSubviews  // keep this node, discard its children
//       case mergeInto(String)
//   }
//
// backed there by (at least) `isTransparentGenericStackWrapper` and
// `isColorBackedByColorView` — `nm`/reflection only gives their *signatures*,
// never their bodies, so `classify` below is this crate's own independently
// reasoned heuristic, not a port of theirs. It is also deliberately more
// conservative than what the signatures alone might suggest: the real
// predicates apparently also weigh geometry (`containsProjectedStackLayout`
// looks like it avoids eliding a wrapper that would destroy a stack
// layout's frame information further down) — this crate's SwiftUI nodes all
// report a zero frame today (a documented, separate gap — see
// `decode_json_node`'s flags:0 handling above), so geometry is not an
// available signal here and isn't used as one. Where the real
// implementation might have geometry to lean on, this one leans on a
// narrower, exact-match type-name allowlist instead, on the theory that
// under-eliding (a few extra harmless wrapper nodes stay visible) is a far
// safer failure mode for a debugging tool than over-eliding (hiding real
// content).
#[derive(Debug, Clone, PartialEq, Eq)]
enum FilterDecision {
    Keep,
    /// Remove this node; splice its children directly into its parent's
    /// position instead.
    ElideHoist,
    /// Keep this node itself, but discard its children (they're known
    /// internal plumbing, not user content).
    ElideSubviews,
    /// Fold this node's info into a named ancestor group instead of
    /// emitting it as a separate tree node. Never actually produced by
    /// `classify` in this pass — Part B's properties flattener already
    /// folds a node's *own* modifier/attribute detail into `style_rows`,
    /// which covers the cases this crate has evidence for; merging a whole
    /// *separate sibling node's* identity into an ancestor is a different,
    /// riskier operation with no real captured payload to justify a
    /// specific heuristic for yet. Kept only so the enum's shape matches
    /// the real one — nothing constructs this variant today.
    #[allow(dead_code)]
    MergeInto(String),
}

/// Type-name prefixes recognized as purely-structural SwiftUI plumbing that
/// contributes no visual/semantic identity of its own — candidates for
/// `ElideHoist`, and *only* candidates: `classify` additionally requires
/// exactly one child and no modifiers of the wrapper's own before actually
/// eliding one (see its doc comment for why name-matching alone isn't
/// enough). Deliberately excludes real layout containers
/// (`VStack`/`HStack`/`ZStack`/`List`/`ScrollView`/...) even though some of
/// them can also appear with a single child — those are genuine content
/// containers a user would recognize and want to see, not implementation
/// plumbing, so they're never candidates here regardless of child count.
const TRANSPARENT_WRAPPER_PREFIXES: &[&str] = &[
    "_VariadicView.Tree<",
    "_VariadicView_Children",
    "TupleView<",
    "_ConditionalContent<",
    "_UnaryViewAdaptor<",
    "LazyView<",
    "PlaceholderContentView<",
    "AnyView",
];

/// Exact base type names (the part of `class_name` before any generic `<`)
/// recognized as known leaf SwiftUI primitives — candidates for
/// `ElideSubviews`. Exact-match, not prefix/substring match, specifically
/// so this can never accidentally fire on an unrelated type that merely
/// starts with, say, `"Color"` (`ColorPicker`, a real interactive control a
/// user would want to see, is not `"Color"`).
const LEAF_PRIMITIVE_BASE_NAMES: &[&str] = &["Color", "Spacer", "Divider", "EmptyView"];

/// Classifies one already-decoded (and already recursively filtered —
/// `children` here reflects this node's own children *after* their own
/// level's filtering already ran) `ViewNode` for the node-hiding filter.
///
/// `ElideSubviews` is checked first and doesn't require an empty
/// `modifiers` list — hiding a `Color`/`Spacer`/etc.'s internal-plumbing
/// children is safe regardless of whether a modifier was applied to the
/// primitive itself (that modifier detail is unaffected; it's still on
/// this node, which stays in the tree). `ElideHoist` is stricter: it
/// removes the node entirely, so it additionally requires the wrapper to
/// carry no modifiers of its own (losing modifier detail by silently
/// deleting the node that carried it would be exactly the kind of
/// information loss this filter must not cause) and exactly one child
/// (hoisting a multi-child wrapper would ambiguously reparent siblings that
/// were never siblings in the real view tree).
fn classify(node: &ViewNode) -> FilterDecision {
    let base_name = node
        .class_name
        .split('<')
        .next()
        .unwrap_or(&node.class_name);

    if LEAF_PRIMITIVE_BASE_NAMES.contains(&base_name) && !node.children.is_empty() {
        return FilterDecision::ElideSubviews;
    }

    if node.modifiers.is_empty()
        && node.children.len() == 1
        && TRANSPARENT_WRAPPER_PREFIXES
            .iter()
            .any(|prefix| node.class_name.starts_with(prefix))
    {
        return FilterDecision::ElideHoist;
    }

    FilterDecision::Keep
}

/// Escape hatch for A/B comparison and rollback without a rebuild — set to
/// disable the filter entirely and get the pre-filter tree back, e.g. to
/// confirm a specific node's disappearance is actually this filter's doing.
fn filter_disabled() -> bool {
    std::env::var_os("SWIFTUI_DEBUG_DISABLE_NODE_FILTER").is_some()
}

/// Applies `classify` to one node's already-decoded children, producing the
/// filtered child list actually attached to the `ViewNode` being built.
fn apply_view_filter(children: Vec<ViewNode>) -> Vec<ViewNode> {
    apply_view_filter_with(children, filter_disabled())
}

/// The actual filtering logic, taking `disabled` as a plain argument rather
/// than reading the env var itself — specifically so tests can exercise the
/// disabled path directly rather than mutating process-global env state
/// (`std::env::set_var` in one `#[test]` racing another's `filter_disabled()`
/// read, since Rust test binaries run tests concurrently by default).
fn apply_view_filter_with(children: Vec<ViewNode>, disabled: bool) -> Vec<ViewNode> {
    if disabled {
        return children;
    }
    let mut result = Vec::with_capacity(children.len());
    for child in children {
        match classify(&child) {
            FilterDecision::Keep => {
                FILTER_KEPT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                result.push(child);
            }
            FilterDecision::ElideHoist => {
                FILTER_ELIDE_HOIST_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                result.extend(child.children);
            }
            FilterDecision::ElideSubviews => {
                FILTER_ELIDE_SUBVIEWS_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut kept = child;
                kept.children.clear();
                result.push(kept);
            }
            FilterDecision::MergeInto(_) => {
                // Never actually produced by `classify` today — see its
                // doc comment — but handled for completeness rather than
                // left as an unreachable match arm, in case that changes.
                FILTER_MERGE_INTO_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                result.push(child);
            }
        }
    }
    result
}

/// Per-`DOM.getDocument`-call (really: per hosting view — see
/// `decode_json_nodes`, the only caller of `reset_filter_counters`/
/// `log_filter_summary`) counters, so the filter's effect is directly
/// observable via tracing rather than only inferable from tree-size
/// differences. `Relaxed` throughout: these are diagnostic counters, not
/// synchronization.
static DECODED_NODE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static FILTER_KEPT_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static FILTER_ELIDE_HOIST_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static FILTER_ELIDE_SUBVIEWS_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static FILTER_MERGE_INTO_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn reset_filter_counters() {
    use std::sync::atomic::Ordering::Relaxed;
    DECODED_NODE_COUNT.store(0, Relaxed);
    FILTER_KEPT_COUNT.store(0, Relaxed);
    FILTER_ELIDE_HOIST_COUNT.store(0, Relaxed);
    FILTER_ELIDE_SUBVIEWS_COUNT.store(0, Relaxed);
    FILTER_MERGE_INTO_COUNT.store(0, Relaxed);
}

fn count_decoded_node() {
    DECODED_NODE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn log_filter_summary() {
    use std::sync::atomic::Ordering::Relaxed;
    let decoded = DECODED_NODE_COUNT.load(Relaxed);
    let hoisted = FILTER_ELIDE_HOIST_COUNT.load(Relaxed);
    let elided_subviews = FILTER_ELIDE_SUBVIEWS_COUNT.load(Relaxed);
    let merged = FILTER_MERGE_INTO_COUNT.load(Relaxed);
    if hoisted == 0 && elided_subviews == 0 && merged == 0 {
        return;
    }
    tracing::debug!(
        decoded_nodes = decoded,
        kept = FILTER_KEPT_COUNT.load(Relaxed),
        elide_hoist = hoisted,
        elide_subviews = elided_subviews,
        merge_into = merged,
        disabled = filter_disabled(),
        "SwiftUI node-hiding filter summary"
    );
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
    fn verified_os_version_allowlist_is_exact_not_a_range() {
        // Spike-verified: iOS 26.x.
        assert!(is_verified_os_version(26));
        // Never checked at all — must stay gated off until someone actually
        // re-runs the spike and extends the allowlist, not just because the
        // major version number is adjacent to a verified one.
        assert!(!is_verified_os_version(25));
        assert!(!is_verified_os_version(27));
        assert!(!is_verified_os_version(18));
        assert!(!is_verified_os_version(0));
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
        // `class_name` is now just the view's own type — no modifier text
        // appended, matching the CDP `nodeName` the Elements panel displays
        // as the tag name (see `domain_dom.rs`'s `attributes()` for where
        // `modifiers` surfaces instead).
        assert_eq!(nodes[0].class_name, "VStack<TupleView<(Text, Text)>>");
        assert_eq!(nodes[0].modifiers, vec!["_PaddingLayout".to_string()]);
        assert_eq!(nodes[0].children.len(), 1);
        assert_eq!(nodes[0].children[0].class_name, "Text");
        assert!(nodes[0].children[0].modifiers.is_empty());
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
        assert_eq!(nodes[0].class_name, "Text");
        assert_eq!(nodes[0].modifiers, vec!["<unknown modifier>".to_string()]);
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
        // The synthetic `<modifier: ...>` label already carries this
        // information in `class_name` (there's no real view type here for
        // `modifiers` to be "applied to"), so it isn't duplicated into the
        // separate `modifiers` field too.
        assert!(nodes[0].modifiers.is_empty());
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
        // This fragment's own `readableType` is itself a
        // `ModifiedContent<Base, Modifier>` — recursive unwrapping (Part A)
        // now peels that apart into a clean base `class_name` plus a
        // `modifiers` entry, instead of leaving the whole concatenated
        // generic as the tag name.
        assert_eq!(
            nodes[0].class_name,
            "_ConditionalContent<_ViewList_View, TabItemGroup.HostView>"
        );
        assert_eq!(
            nodes[0].modifiers,
            vec!["NavigationSearchAdjustmentModifier".to_string()]
        );
        assert_eq!(nodes[0].children.len(), 1);
        // The child has a modifier (flags:2) property and no flags:1 view
        // type — the synthetic `<modifier: ...>` label case again.
        assert!(nodes[0].children[0]
            .class_name
            .contains("NavigationSearchAdjustmentModifier"));
    }

    // -----------------------------------------------------------------
    // Part A — recursive ModifiedContent unwrapping + length cap.
    // -----------------------------------------------------------------

    #[test]
    fn unwraps_a_single_level_of_modified_content() {
        let (base, modifiers) = unwrap_modified_content(
            "ModifiedContent<VStack<TupleView<(Text, Text)>>, _PaddingLayout>",
        );
        assert_eq!(base, "VStack<TupleView<(Text, Text)>>");
        assert_eq!(modifiers, vec!["_PaddingLayout".to_string()]);
    }

    #[test]
    fn unwraps_multiple_nested_levels_recursively() {
        let (base, modifiers) = unwrap_modified_content(
            "ModifiedContent<ModifiedContent<Text, _EnvironmentKeyWritingModifier<ContentTransition>>, _AnimationModifier<Int>>",
        );
        assert_eq!(base, "Text");
        assert_eq!(
            modifiers,
            vec![
                "_AnimationModifier<Int>".to_string(),
                "_EnvironmentKeyWritingModifier<ContentTransition>".to_string(),
            ]
        );
    }

    #[test]
    fn non_modified_content_type_is_left_completely_alone() {
        let (base, modifiers) = unwrap_modified_content("VStack<TupleView<(Text, Text)>>");
        assert_eq!(base, "VStack<TupleView<(Text, Text)>>");
        assert!(modifiers.is_empty());
    }

    #[test]
    fn tolerates_a_module_qualified_modified_content_name() {
        let (base, modifiers) = unwrap_modified_content(
            "SwiftUI.ModifiedContent<SwiftUI.Text, SwiftUI._PaddingLayout>",
        );
        assert_eq!(base, "SwiftUI.Text");
        assert_eq!(modifiers, vec!["SwiftUI._PaddingLayout".to_string()]);
    }

    #[test]
    fn a_malformed_or_unrecognized_shape_is_never_force_unwrapped() {
        // Three top-level generic args, not the two a real `ModifiedContent`
        // always has — must be left alone rather than guessing which two to
        // treat as (Base, Modifier).
        let (base, modifiers) = unwrap_modified_content("ModifiedContent<A, B, C>");
        assert_eq!(base, "ModifiedContent<A, B, C>");
        assert!(modifiers.is_empty());
    }

    #[test]
    fn cap_display_length_only_truncates_when_over_the_limit() {
        assert_eq!(cap_display_length("short", 200), "short");
        let exactly_at_limit = "a".repeat(200);
        assert_eq!(cap_display_length(&exactly_at_limit, 200), exactly_at_limit);
        let over_limit = "a".repeat(250);
        let capped = cap_display_length(&over_limit, 200);
        assert_eq!(capped.chars().count(), 201); // 200 chars + the "…" marker
        assert!(capped.ends_with('…'));
    }

    /// The actual 3018-character `readableType` captured live from this
    /// demo app's own `TabHostingController` hosting view (iOS 26.2
    /// simulator, `SWIFTUI_DEBUG_DUMP_DIR`) — not a synthetic
    /// approximation. Confirms the real worst-case node this pass was
    /// built to fix: recursive unwrapping alone still leaves a huge base
    /// (`NavigationStackStyledCore<...>`, itself wrapping a whole `VStack`
    /// tree), so the hard length cap is what actually keeps the tag name
    /// short here, not the unwrapping alone.
    const REAL_WORST_CASE_READABLE_TYPE: &str = "ModifiedContent<ModifiedContent<NavigationStackStyledCore<ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<VStack<TupleView<(ConnectionBadge, Spacer, Text, ModifiedContent<ModifiedContent<Text, _EnvironmentKeyWritingModifier<ContentTransition>>, _AnimationModifier<Int>>, ModifiedContent<ModifiedContent<Text, _EnvironmentKeyWritingModifier<Optional<Text.Case>>>, _EnvironmentKeyWritingModifier<CGFloat>>, HStack<TupleView<(CounterButton, CounterButton, CounterButton)>>, Spacer, Optional<ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<HStack<TupleView<(Image, Text)>>, _EnvironmentKeyWritingModifier<Optional<Font>>>, _ForegroundStyleModifier<HierarchicalShapeStyle>>, _PaddingLayout>, _PaddingLayout>, _InsettableBackgroundShapeModifier<Material, Capsule>>>)>>, _PaddingLayout>, TransactionalPreferenceTransformModifier<NavigationTitleKey>>, _PreferenceTransformModifier<ToolbarKey>>, _TaskModifier>, NavigationStackRootDecoratingModifier>>, PositionedNavigationDestinationProcessor<NavigationStackReader<NavigationStackStyledCore<ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<VStack<TupleView<(ConnectionBadge, Spacer, Text, ModifiedContent<ModifiedContent<Text, _EnvironmentKeyWritingModifier<ContentTransition>>, _AnimationModifier<Int>>, ModifiedContent<ModifiedContent<Text, _EnvironmentKeyWritingModifier<Optional<Text.Case>>>, _EnvironmentKeyWritingModifier<CGFloat>>, HStack<TupleView<(CounterButton, CounterButton, CounterButton)>>, Spacer, Optional<ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<HStack<TupleView<(Image, Text)>>, _EnvironmentKeyWritingModifier<Optional<Font>>>, _ForegroundStyleModifier<HierarchicalShapeStyle>>, _PaddingLayout>, _PaddingLayout>, _InsettableBackgroundShapeModifier<Material, Capsule>>>)>>, _PaddingLayout>, TransactionalPreferenceTransformModifier<NavigationTitleKey>>, _PreferenceTransformModifier<ToolbarKey>>, _TaskModifier>, NavigationStackRootDecoratingModifier>>, ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<VStack<TupleView<(ConnectionBadge, Spacer, Text, ModifiedContent<ModifiedContent<Text, _EnvironmentKeyWritingModifier<ContentTransition>>, _AnimationModifier<Int>>, ModifiedContent<ModifiedContent<Text, _EnvironmentKeyWritingModifier<Optional<Text.Case>>>, _EnvironmentKeyWritingModifier<CGFloat>>, HStack<TupleView<(CounterButton, CounterButton, CounterButton)>>, Spacer, Optional<ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<ModifiedContent<HStack<TupleView<(Image, Text)>>, _EnvironmentKeyWritingModifier<Optional<Font>>>, _ForegroundStyleModifier<HierarchicalShapeStyle>>, _PaddingLayout>, _PaddingLayout>, _InsettableBackgroundShapeModifier<Material, Capsule>>>)>>, _PaddingLayout>, TransactionalPreferenceTransformModifier<NavigationTitleKey>>, _PreferenceTransformModifier<ToolbarKey>>, _TaskModifier>>.AppliedBody>>, _PreferenceTransformModifier<InspectorStorageV5.PreferenceKey>>";

    #[test]
    fn real_3018_char_worst_case_node_unwraps_and_caps_to_a_short_tag_name() {
        assert_eq!(REAL_WORST_CASE_READABLE_TYPE.chars().count(), 3018);
        let (base, modifiers) = unwrap_modified_content(REAL_WORST_CASE_READABLE_TYPE);
        // Two levels of ModifiedContent peeled off the outside; the base
        // left over (a NavigationStackStyledCore wrapping a whole VStack
        // tree) is still huge on its own — recursive unwrapping alone does
        // NOT fully solve this real case, which is exactly why the hard
        // cap below exists.
        assert_eq!(modifiers.len(), 2);
        assert!(base.starts_with("NavigationStackStyledCore<"));
        assert!(
            base.chars().count() > MAX_CLASS_NAME_CHARS,
            "this is the real case the cap exists for — expected the unwrapped base to still be huge"
        );

        let capped = cap_display_length(&base, MAX_CLASS_NAME_CHARS);
        assert_eq!(capped.chars().count(), MAX_CLASS_NAME_CHARS + 1);
        assert!(capped.ends_with('…'));

        // End-to-end through the actual decode path, not just the two
        // helpers in isolation.
        let json = serde_json::json!([{
            "properties": [
                { "id": 0, "attribute": { "flags": 1, "readableType": REAL_WORST_CASE_READABLE_TYPE } }
            ],
            "children": []
        }]);
        let nodes = decode_json_nodes(&json).expect("should decode");
        assert!(nodes[0].class_name.chars().count() <= MAX_CLASS_NAME_CHARS + 1);
        assert!(nodes[0]
            .class_name
            .starts_with("NavigationStackStyledCore<"));
        assert!(nodes[0].class_name.ends_with('…'));
        assert_eq!(nodes[0].modifiers.len(), 2);
    }

    // -----------------------------------------------------------------
    // Part B — bounded properties flattener.
    // -----------------------------------------------------------------

    #[test]
    fn flattens_a_scalar_subattribute_into_a_row() {
        let properties = serde_json::json!([
            {
                "attribute": {
                    "type": "SwiftUI.NavigationSearchAdjustmentModifier",
                    "readableType": "NavigationSearchAdjustmentModifier",
                    "flags": 0,
                    "subattributes": [
                        {
                            "name": "searchAdjustment",
                            "readableType": "SearchAdjustment",
                            "value": "disabled",
                            "type": "SwiftUI.SearchAdjustment",
                            "flags": 0
                        }
                    ]
                }
            }
        ]);
        let rows = flatten_node_properties(properties.as_array().unwrap(), 8, 50);
        assert_eq!(
            rows,
            vec![("searchAdjustment".to_string(), "disabled".to_string())]
        );
    }

    #[test]
    fn leaf_attribute_with_no_value_falls_back_to_its_own_type_as_the_value() {
        let properties = serde_json::json!([
            {
                "attribute": {
                    "type": "SwiftUI._PaddingLayout",
                    "readableType": "_PaddingLayout",
                    "flags": 2
                }
            }
        ]);
        let rows = flatten_node_properties(properties.as_array().unwrap(), 8, 50);
        assert_eq!(
            rows,
            vec![("_PaddingLayout".to_string(), "_PaddingLayout".to_string())]
        );
    }

    #[test]
    fn flattened_row_title_and_value_are_capped_like_class_name_is() {
        // Confirmed live: a `flags:1` attribute with no `name_hint`, no
        // scalar value, and no subattributes otherwise produces a row whose
        // title *and* value are both its own type string verbatim — for a
        // deeply-nested-generic SwiftUI type, that's the exact same
        // unreadable-length problem `class_name` has, just restated as a
        // `--swiftui-<huge>` custom property in the Styles pane instead.
        let huge_type = format!("VStack<{}>", "Text, ".repeat(60));
        assert!(huge_type.chars().count() > FLATTEN_ROW_TEXT_CHAR_LIMIT);
        let properties = serde_json::json!([
            { "attribute": { "type": huge_type.clone(), "readableType": huge_type, "flags": 1 } }
        ]);
        let rows = flatten_node_properties(properties.as_array().unwrap(), 8, 50);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].0.chars().count() <= FLATTEN_ROW_TEXT_CHAR_LIMIT + 1);
        assert!(rows[0].1.chars().count() <= FLATTEN_ROW_TEXT_CHAR_LIMIT + 1);
        assert!(rows[0].0.ends_with('…'));
        assert!(rows[0].1.ends_with('…'));
    }

    #[test]
    fn row_cap_truncates_and_reports_a_note() {
        // 5 top-level properties, each a leaf with no value/subattributes.
        let properties: Vec<serde_json::Value> = (0..5)
            .map(|i| {
                serde_json::json!({
                    "attribute": {
                        "type": format!("SwiftUI.Thing{i}"),
                        "readableType": format!("Thing{i}"),
                        "flags": 2
                    }
                })
            })
            .collect();
        let rows = flatten_node_properties(&properties, 8, 2);
        // 2 real rows (the cap) + 1 truncation-note row.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], ("Thing0".to_string(), "Thing0".to_string()));
        assert_eq!(rows[1], ("Thing1".to_string(), "Thing1".to_string()));
        assert_eq!(rows[2].0, "…");
        assert!(rows[2].1.contains("more (truncated)"));
    }

    #[test]
    fn depth_limit_truncates_deeply_nested_subattributes_and_reports_a_note() {
        // Build a chain of subattributes nested 5 deep; a depth_limit of 2
        // should stop partway through and report the cut-off.
        fn nested(depth: usize) -> serde_json::Value {
            if depth == 0 {
                serde_json::json!({
                    "name": "leaf",
                    "type": "SwiftUI.Leaf",
                    "readableType": "Leaf",
                    "flags": 0,
                    "value": "bottom"
                })
            } else {
                serde_json::json!({
                    "name": format!("level{depth}"),
                    "type": "SwiftUI.Level",
                    "readableType": "Level",
                    "flags": 0,
                    "subattributes": [nested(depth - 1)]
                })
            }
        }
        let properties = vec![serde_json::json!({ "attribute": nested(5) })];
        let rows = flatten_node_properties(&properties, 2, 50);
        // Row cap wasn't hit; depth limit was — still expect a truncation
        // note, not a silent drop.
        assert!(rows.iter().any(|(title, _)| title == "…"));
    }

    #[test]
    fn no_truncation_note_when_nothing_was_cut_off() {
        let properties = serde_json::json!([
            { "attribute": { "type": "SwiftUI.Text", "readableType": "Text", "flags": 1 } }
        ]);
        let rows = flatten_node_properties(properties.as_array().unwrap(), 8, 50);
        assert!(!rows.iter().any(|(title, _)| title == "…"));
    }

    // -----------------------------------------------------------------
    // Node-hiding filter (classify / apply_view_filter).
    // -----------------------------------------------------------------

    fn node(class_name: &str, modifiers: Vec<&str>, children: Vec<ViewNode>) -> ViewNode {
        ViewNode {
            class_name: class_name.to_string(),
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
            modifiers: modifiers.into_iter().map(str::to_string).collect(),
            style_rows: Vec::new(),
            children,
        }
    }

    fn leaf_node(class_name: &str) -> ViewNode {
        node(class_name, vec![], vec![])
    }

    #[test]
    fn transparent_wrapper_with_one_child_and_no_modifiers_is_elide_hoist() {
        let wrapper = node(
            "_VariadicView.Tree<_VStackLayout, _VariadicView_Children>",
            vec![],
            vec![leaf_node("Text")],
        );
        assert_eq!(classify(&wrapper), FilterDecision::ElideHoist);
    }

    #[test]
    fn transparent_wrapper_with_two_children_is_kept_not_hoisted() {
        // Hoisting a multi-child wrapper would ambiguously reparent
        // siblings that were never siblings in the real view tree.
        let wrapper = node(
            "_VariadicView.Tree<_VStackLayout, _VariadicView_Children>",
            vec![],
            vec![leaf_node("Text"), leaf_node("Image")],
        );
        assert_eq!(classify(&wrapper), FilterDecision::Keep);
    }

    #[test]
    fn transparent_wrapper_carrying_its_own_modifier_is_kept_not_hoisted() {
        // Eliding this would silently delete the modifier detail it
        // carries — never allowed, regardless of child count.
        let wrapper = node("AnyView", vec!["_PaddingLayout"], vec![leaf_node("Text")]);
        assert_eq!(classify(&wrapper), FilterDecision::Keep);
    }

    #[test]
    fn real_layout_container_is_never_a_hoist_candidate_even_with_one_child() {
        // The exact case this filter must never do: a genuine content
        // container (VStack/HStack/ZStack/...) is not "plumbing" just
        // because it happens to have a single child in this particular
        // screen — a user would recognize and want to see it.
        for name in [
            "VStack<Text>",
            "HStack<Text>",
            "ZStack<Text>",
            "List<Text>",
            "ScrollView<Text>",
        ] {
            let container = node(name, vec![], vec![leaf_node("Text")]);
            assert_eq!(
                classify(&container),
                FilterDecision::Keep,
                "{name} must never be hoisted"
            );
        }
    }

    #[test]
    fn known_leaf_primitive_with_children_elides_its_subviews() {
        for name in ["Color", "Spacer", "Divider", "EmptyView"] {
            let primitive = node(name, vec![], vec![leaf_node("_InternalPlumbing")]);
            assert_eq!(
                classify(&primitive),
                FilterDecision::ElideSubviews,
                "{name} should elide its internal-plumbing children"
            );
        }
    }

    #[test]
    fn leaf_primitive_name_match_is_exact_not_substring() {
        // A real interactive control that merely starts with "Color" must
        // never be mistaken for the bare `Color` primitive.
        let picker = node("ColorPicker<Label>", vec![], vec![leaf_node("_Internal")]);
        assert_eq!(classify(&picker), FilterDecision::Keep);
    }

    #[test]
    fn leaf_primitive_with_no_children_has_nothing_to_elide() {
        let primitive = leaf_node("Color");
        assert_eq!(classify(&primitive), FilterDecision::Keep);
    }

    #[test]
    fn apply_view_filter_splices_hoisted_children_into_the_parent_list() {
        // The wrapper has exactly its one required child (a real
        // multi-child container is never a hoist candidate — see
        // `transparent_wrapper_with_two_children_is_kept_not_hoisted`);
        // that single child itself has its own children, which must all
        // still be present after hoisting.
        let grandchild = leaf_node("Image");
        let wrapper_child = node("Text", vec![], vec![grandchild]);
        let wrapper = node(
            "_VariadicView.Tree<_VStackLayout, _VariadicView_Children>",
            vec![],
            vec![wrapper_child],
        );
        let sibling = leaf_node("Button");
        let filtered = apply_view_filter(vec![wrapper, sibling]);
        // The wrapper itself is gone; its one real child takes its place
        // among its siblings, still carrying its own grandchild.
        let names: Vec<&str> = filtered.iter().map(|n| n.class_name.as_str()).collect();
        assert_eq!(names, vec!["Text", "Button"]);
        assert_eq!(filtered[0].children.len(), 1);
        assert_eq!(filtered[0].children[0].class_name, "Image");
    }

    #[test]
    fn apply_view_filter_clears_children_for_elide_subviews_but_keeps_the_node() {
        let primitive = node("Spacer", vec![], vec![leaf_node("_InternalPlumbing")]);
        let filtered = apply_view_filter(vec![primitive]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].class_name, "Spacer");
        assert!(filtered[0].children.is_empty());
    }

    #[test]
    fn apply_view_filter_disabled_is_a_full_bypass() {
        // Exercises the `disabled` branch directly via
        // `apply_view_filter_with`, rather than mutating the real
        // `SWIFTUI_DEBUG_DISABLE_NODE_FILTER` process env var — tests run
        // concurrently by default, and a shared env var would race against
        // every other test in this module that (indirectly, through
        // `apply_view_filter`/`filter_disabled`) reads it.
        let wrapper = node(
            "_VariadicView.Tree<_VStackLayout, _VariadicView_Children>",
            vec![],
            vec![leaf_node("Text")],
        );
        let filtered = apply_view_filter_with(vec![wrapper], true);
        // Bypassed entirely — the wrapper itself is still present, unhoisted.
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].class_name.starts_with("_VariadicView.Tree"));
    }

    /// A real captured fragment (see `decodes_a_real_captured_fragment_with_subattributes`
    /// above for provenance) exercised end-to-end through `decode_json_nodes`:
    /// its child is a bare `NavigationSearchAdjustmentModifier` label with no
    /// children of its own, so no filter decision beyond `Keep` applies —
    /// confirms the filter doesn't disturb a payload shape already covered
    /// by other tests.
    #[test]
    fn real_captured_fragment_is_unaffected_by_the_node_filter() {
        let json = serde_json::json!([{
            "properties": [
                { "id": 0, "attribute": { "flags": 1, "readableType": "_ConditionalContent<_ViewList_View, TabItemGroup.HostView>" } }
            ],
            "children": [
                {
                    "properties": [
                        { "attribute": { "type": "SwiftUI.NavigationSearchAdjustmentModifier", "readableType": "NavigationSearchAdjustmentModifier", "flags": 2 } }
                    ],
                    "children": []
                }
            ]
        }]);
        let nodes = decode_json_nodes(&json).expect("should decode");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].children.len(), 1);
    }
}
