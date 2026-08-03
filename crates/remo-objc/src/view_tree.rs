//! ObjC runtime bridge for inspecting iOS view hierarchy.
//!
//! This module uses `objc2` to walk the UIView tree and serialize it
//! into a JSON-friendly structure. On non-Apple targets, all functions
//! are stubbed out.

use serde::{Deserialize, Serialize};

/// A node in the view hierarchy tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewNode {
    pub class_name: String,
    pub frame: Frame,
    pub is_hidden: bool,
    pub alpha: f64,
    pub tag: isize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessibility_id: Option<String>,
    pub children: Vec<ViewNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

// ---------------------------------------------------------------------------
// Apple target implementation
// ---------------------------------------------------------------------------

#[cfg(all(target_vendor = "apple", feature = "uikit"))]
mod apple {
    use super::*;
    use objc2::runtime::AnyObject;
    use objc2::{msg_send, MainThreadMarker};
    use objc2_foundation::NSString;
    use objc2_ui_kit::{UIApplication, UIView};

    /// Snapshot the entire view tree starting from the key window.
    ///
    /// # Safety
    /// Must be called on the main thread.
    pub unsafe fn snapshot_key_window() -> Option<ViewNode> {
        let mtm = MainThreadMarker::new_unchecked();
        let app = UIApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        let windows = app.windows();
        let key_window = windows.iter().find(|w| w.isKeyWindow());

        key_window.map(|w| walk_view(&w))
    }

    /// Recursively walk a UIView and its subviews.
    unsafe fn walk_view(view: &UIView) -> ViewNode {
        let class_name = view
            .class()
            .name()
            .to_str()
            .unwrap_or("<unknown>")
            .to_owned();

        let frame = view.frame();
        let subviews = view.subviews();

        let accessibility_id: Option<String> = {
            let raw: *mut AnyObject = msg_send![view, accessibilityIdentifier];
            if raw.is_null() {
                None
            } else {
                let ns: &NSString = &*(raw as *const NSString);
                Some(ns.to_string())
            }
        };

        let children: Vec<ViewNode> = subviews.iter().map(|sv| walk_view(&sv)).collect();

        ViewNode {
            class_name,
            frame: Frame {
                x: frame.origin.x,
                y: frame.origin.y,
                width: frame.size.width,
                height: frame.size.height,
            },
            is_hidden: view.isHidden(),
            alpha: view.alpha() as f64,
            tag: view.tag(),
            accessibility_id,
            children,
        }
    }
}

// ---------------------------------------------------------------------------
// Stub for non-Apple targets (so the workspace compiles on Linux/CI)
// ---------------------------------------------------------------------------

#[cfg(not(all(target_vendor = "apple", feature = "uikit")))]
mod apple {
    use super::*;

    #[allow(clippy::unnecessary_wraps)]
    pub unsafe fn snapshot_key_window() -> Option<ViewNode> {
        tracing::warn!("snapshot_key_window called on non-Apple target, returning stub");
        Some(ViewNode {
            class_name: "StubView".into(),
            frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 375.0,
                height: 812.0,
            },
            is_hidden: false,
            alpha: 1.0,
            tag: 0,
            accessibility_id: None,
            children: vec![],
        })
    }
}

/// Public API: snapshot the key window's view tree.
///
/// # Safety
/// On iOS, must be called from the main thread.
pub unsafe fn snapshot_view_tree() -> Option<ViewNode> {
    apple::snapshot_key_window()
}
