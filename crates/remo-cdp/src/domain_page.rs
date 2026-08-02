//! Track B: real Chrome DevTools frontend compatibility for `Page`-adjacent
//! bootstrap calls, plus the standard low-fidelity screencast path.
//!
//! A real Chrome frontend (`chrome://inspect`) fires a fixed bootstrap
//! sequence of `enable`/`get*` calls across many domains regardless of which
//! panel the user opens, and an unshaped or missing reply to several
//! specific ones throws inside the frontend and kills the whole session —
//! this was empirically confirmed against a real Chrome 150 frontend while
//! building a similar Swift CDP server for a different app. Every method
//! claimed here exists because it is one of those "throws if wrong shape"
//! calls, or because it is the actual screenshot/screencast feature.
//!
//! This is deliberately *not* Remo's existing high-fidelity H.264 mirror —
//! that's a documented future extension. This is the standard JPEG-per-frame
//! screencast that feeds `chrome://inspect`'s own built-in device-mirror
//! panel, reusing the same `remo_objc::capture_screenshot` path as
//! `Page.captureScreenshot`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use tokio::time::interval;

use crate::dispatcher::{CdpDomain, CdpReply, CdpRequest, EventSink};
use crate::remote_object;

/// How often screencast frames are captured and emitted. `Page.startScreencast`
/// accepts an `everyNthFrame` param in real CDP, but there's no underlying
/// frame source to subsample here (each "frame" is its own fresh capture) —
/// a fixed interval is the honest first-pass answer.
const SCREENCAST_INTERVAL: Duration = Duration::from_millis(300);

/// `Page.captureScreenshot`/`Page.startScreencast`/`Page.stopScreencast`,
/// `Network.emulateNetworkConditionsByRule`, `Runtime.callFunctionOn`, and the
/// `CSS.get*`/`CSS.takeComputedStyleUpdates` bootstrap stubs the Elements
/// and Computed panels poll.
///
/// Holds the in-flight screencast task (if any) behind a `Mutex` since
/// [`CdpDomain::respond`]/[`CdpDomain::reset`] take `&self`, not `&mut self`
/// — this is the one piece of genuinely per-connection mutable state this
/// domain owns.
pub struct PageDomain {
    screencast: Mutex<Option<JoinHandle<()>>>,
    next_session_id: AtomicU32,
}

impl PageDomain {
    pub fn new() -> Self {
        Self {
            screencast: Mutex::new(None),
            next_session_id: AtomicU32::new(1),
        }
    }

    /// Captures one screenshot on tokio's blocking pool.
    ///
    /// `remo_objc::run_on_main_sync` blocks synchronously until the main
    /// thread services it — calling it directly on a tokio worker thread
    /// would risk starving the async runtime under concurrent load, so the
    /// whole main-thread round-trip runs via `spawn_blocking` on tokio's
    /// dedicated blocking-thread pool instead.
    #[allow(unsafe_code)]
    async fn capture(format: String, quality: f64) -> Option<remo_objc::ScreenshotResult> {
        tokio::task::spawn_blocking(move || {
            remo_objc::run_on_main_sync(|| {
                // SAFETY: `run_on_main_sync` guarantees this closure runs on
                // the main thread, which is what UIKit's rendering calls
                // (`capture_screenshot` uses `UIGraphicsBeginImageContext`/
                // `drawViewHierarchyInRect:afterScreenUpdates:`) require.
                unsafe { remo_objc::capture_screenshot(&format, quality) }
            })
        })
        .await
        .unwrap_or_default()
    }

    async fn capture_screenshot(request: &CdpRequest) -> CdpReply {
        let format = request
            .params
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("jpeg")
            .to_string();
        let quality = request
            .params
            .get("quality")
            .and_then(Value::as_u64)
            .map_or(0.8, |q| q as f64 / 100.0);

        match Self::capture(format, quality).await {
            Some(result) => CdpReply::ok(json!({ "data": BASE64.encode(result.bytes) })),
            None => CdpReply::error("screenshot capture failed"),
        }
    }

    fn start_screencast(&self, request: &CdpRequest, events: &EventSink) -> CdpReply {
        let format = request
            .params
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("jpeg")
            .to_string();
        let quality = request
            .params
            .get("quality")
            .and_then(Value::as_u64)
            .map_or(0.8, |q| q as f64 / 100.0);
        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let events = events.clone();

        let handle = tokio::spawn(async move {
            let mut ticker = interval(SCREENCAST_INTERVAL);
            loop {
                ticker.tick().await;
                let Some(result) = Self::capture(format.clone(), quality).await else {
                    continue;
                };
                events.emit(
                    "Page.screencastFrame",
                    &json!({
                        "data": BASE64.encode(result.bytes),
                        "metadata": {
                            "deviceWidth": result.width,
                            "deviceHeight": result.height,
                        },
                        "sessionId": session_id,
                    }),
                );
            }
        });

        self.abort_screencast_locked(Some(handle));
        CdpReply::empty()
    }

    fn stop_screencast(&self) -> CdpReply {
        self.abort_screencast_locked(None);
        CdpReply::empty()
    }

    /// Replaces the stored task with `replacement`, aborting whatever was
    /// there before (a no-op if there was nothing).
    fn abort_screencast_locked(&self, replacement: Option<JoinHandle<()>>) {
        let mut slot = self
            .screencast
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = slot.take() {
            previous.abort();
        }
        *slot = replacement;
    }
}

impl Default for PageDomain {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CdpDomain for PageDomain {
    fn methods(&self) -> &'static [&'static str] {
        &[
            "Page.captureScreenshot",
            "Page.startScreencast",
            "Page.stopScreencast",
            "Network.emulateNetworkConditionsByRule",
            "Runtime.callFunctionOn",
            "CSS.getPlatformFontsForNode",
            "CSS.getEnvironmentVariables",
            "CSS.takeComputedStyleUpdates",
        ]
    }

    async fn respond(&self, request: &CdpRequest, events: &EventSink) -> CdpReply {
        match request.method.as_str() {
            "Page.captureScreenshot" => Self::capture_screenshot(request).await,
            "Page.startScreencast" => self.start_screencast(request, events),
            "Page.stopScreencast" => self.stop_screencast(),
            "Network.emulateNetworkConditionsByRule" => CdpReply::ok(json!({ "ruleIds": [] })),
            "Runtime.callFunctionOn" => {
                CdpReply::ok(json!({ "result": remote_object::remote_object(&json!({})) }))
            }
            "CSS.getPlatformFontsForNode" => CdpReply::ok(json!({ "fonts": [] })),
            "CSS.getEnvironmentVariables" => CdpReply::ok(json!({ "environmentVariables": {} })),
            "CSS.takeComputedStyleUpdates" => CdpReply::ok(json!({ "nodeIds": [] })),
            other => CdpReply::error(format!("Page domain does not handle {other}")),
        }
    }

    fn reset(&self) {
        self.abort_screencast_locked(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::EventSink;

    fn request(method: &str, params: Value) -> CdpRequest {
        CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
        }
    }

    fn assert_ok(reply: CdpReply) -> Value {
        match reply {
            CdpReply::Result(value) => value,
            CdpReply::Error { message, .. } => panic!("expected Ok reply, got error: {message}"),
        }
    }

    #[tokio::test]
    async fn emulate_network_conditions_returns_rule_ids_array() {
        let domain = PageDomain::new();
        let (events, _rx) = EventSink::new();
        let reply = domain
            .respond(
                &request("Network.emulateNetworkConditionsByRule", json!({})),
                &events,
            )
            .await;
        assert_eq!(assert_ok(reply), json!({ "ruleIds": [] }));
    }

    #[tokio::test]
    async fn call_function_on_returns_shaped_remote_object() {
        let domain = PageDomain::new();
        let (events, _rx) = EventSink::new();
        let reply = domain
            .respond(&request("Runtime.callFunctionOn", json!({})), &events)
            .await;
        let value = assert_ok(reply);
        assert!(
            value.get("result").is_some(),
            "reply must include result.objectId shape"
        );
        let result = &value["result"];
        assert_eq!(result["type"], "object");
    }

    #[tokio::test]
    async fn get_platform_fonts_for_node_returns_empty_fonts() {
        let domain = PageDomain::new();
        let (events, _rx) = EventSink::new();
        let reply = domain
            .respond(&request("CSS.getPlatformFontsForNode", json!({})), &events)
            .await;
        assert_eq!(assert_ok(reply), json!({ "fonts": [] }));
    }

    #[tokio::test]
    async fn get_environment_variables_returns_empty_map() {
        let domain = PageDomain::new();
        let (events, _rx) = EventSink::new();
        let reply = domain
            .respond(&request("CSS.getEnvironmentVariables", json!({})), &events)
            .await;
        assert_eq!(assert_ok(reply), json!({ "environmentVariables": {} }));
    }

    #[tokio::test]
    async fn take_computed_style_updates_is_stable_across_repeated_polls() {
        let domain = PageDomain::new();
        let (events, _rx) = EventSink::new();
        for _ in 0..3 {
            let reply = domain
                .respond(&request("CSS.takeComputedStyleUpdates", json!({})), &events)
                .await;
            assert_eq!(assert_ok(reply), json!({ "nodeIds": [] }));
        }
    }

    #[tokio::test]
    async fn methods_list_matches_exactly_what_this_domain_claims() {
        let domain = PageDomain::new();
        assert_eq!(
            domain.methods(),
            &[
                "Page.captureScreenshot",
                "Page.startScreencast",
                "Page.stopScreencast",
                "Network.emulateNetworkConditionsByRule",
                "Runtime.callFunctionOn",
                "CSS.getPlatformFontsForNode",
                "CSS.getEnvironmentVariables",
                "CSS.takeComputedStyleUpdates",
            ]
        );
    }
}
