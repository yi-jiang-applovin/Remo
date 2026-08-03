//! `NSUserDefaults` → Chrome DevTools' Application → Storage panel.
//!
//! Ported from the `DOMStorage.*`/`Storage.*` half of Gist-iOS's
//! `DebugCDPStorageMirror.swift` (`PulseDevTools`) — same empirically-tested
//! wire shapes, reduced to the one store Remo currently exposes generically:
//! `NSUserDefaults`' standard domain. That store is modeled as exactly one
//! "local storage" bucket, addressed by the fixed origin
//! `userdefaults://standard` (Gist's MMKV-namespace-per-bucket idea doesn't
//! apply here — there is only one domain to show).
//!
//! `domain_dom::DomDomain` already owns `Page.getResourceTree` (it answers
//! for the "one document" the whole CDP surface hangs off of), so this
//! module does not claim it. Instead the UserDefaults bucket is added there
//! as a child frame — see the `USERDEFAULTS_ORIGIN` wiring in
//! `domain_dom.rs`'s `Page.getResourceTree` — because the Application
//! panel's storage sidebar discovers origins from the frame tree, not from
//! any `DOMStorage.*` method.
//!
//! # v1 non-goals (carried over deliberately, not silently dropped)
//!
//! - **No live external-change polling.** The Swift reference re-dumps every
//!   *opened* bucket on a 3 s timer plus a `UserDefaults.didChangeNotification`
//!   observer, so a value changed via `Remo.invoke`'s `userDefaults.set` (or
//!   by the app itself) while the panel is open shows up without a manual
//!   refresh. This port does not do that yet — mutations made *through this
//!   domain* (`setDOMStorageItem`/`removeDOMStorageItem`/`clear`) do emit the
//!   matching `domStorageItem*` event so the panel updates itself
//!   immediately, but an external change is only picked up on the next
//!   `getDOMStorageItems` (i.e. re-selecting the bucket, or `Cmd+R`). Adding
//!   the polling loop is a follow-up, not a correctness bug in what's here.
//! - **No session storage.** `NSUserDefaults` has no session-scoped concept
//!   equivalent to `sessionStorage`; `isLocalStorage: false` requests answer
//!   with an empty entry list rather than an error, matching real Chrome's
//!   own behavior for an origin with no session storage.
//! - **IndexedDB/CacheStorage are not implemented.** `Storage.setStorageBucketTracking`
//!   is acked but deliberately does *not* emit
//!   `Storage.storageBucketCreatedOrUpdated` — that event is what makes a
//!   real Chrome frontend start probing `IndexedDB.requestDatabaseNames`,
//!   and answering nothing there would be worse than never being asked. A
//!   follow-up implementing IndexedDB (e.g. over `sqlite.query` against
//!   `.sqlite`/`.db` files discovered via `filesystem.list`, tables as
//!   object stores) should emit that event once it exists.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::dispatcher::{CdpDomain, CdpReply, CdpRequest, EventSink};

/// The one storage bucket this domain answers for. Exposed so
/// `domain_dom.rs` can add the matching child frame to `Page.getResourceTree`
/// without this module needing to know anything about frame trees.
pub const USERDEFAULTS_ORIGIN: &str = "userdefaults://standard";

fn storage_key(origin: &str) -> String {
    format!("{origin}/")
}

/// `true` if `storage_id` names the one bucket this domain knows about.
/// Chrome sends `storageKey` on modern requests and `securityOrigin` on
/// legacy ones; either is accepted, matching the Swift reference's routing
/// (storageKey preferred, securityOrigin as fallback).
fn is_userdefaults_bucket(storage_id: &Value) -> bool {
    let key = storage_id
        .get("storageKey")
        .and_then(Value::as_str)
        .or_else(|| storage_id.get("securityOrigin").and_then(Value::as_str))
        .unwrap_or("");
    let key = key.strip_suffix('/').unwrap_or(key);
    key == USERDEFAULTS_ORIGIN
}

fn is_local_storage(storage_id: &Value) -> bool {
    storage_id
        .get("isLocalStorage")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn storage_id_json() -> Value {
    json!({
        "securityOrigin": USERDEFAULTS_ORIGIN,
        "storageKey": storage_key(USERDEFAULTS_ORIGIN),
        "isLocalStorage": true,
    })
}

/// A property-list value as the plain string DevTools' Local Storage table
/// expects (it is a text-only grid): strings pass through unchanged, every
/// other JSON type (number/bool/array/object/null) is compactly JSON-encoded
/// so the table shows something legible instead of Rust's `Display` of a
/// `serde_json::Value`.
fn display_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Inverse of [`display_value`] for writes coming back from the panel: a
/// user typing `42`, `true`, `["a","b"]`, or `{"x":1}` into the value cell
/// should round-trip as that type, not as the literal string `"42"` — so a
/// successful JSON parse wins; anything that fails to parse (e.g. `hello`,
/// not valid JSON) is stored as a plain string, which is the only type a
/// non-JSON string could have meant.
fn parse_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// `DOMStorage.*`/`Storage.*` for the one generic key-value store Remo
/// exposes: `NSUserDefaults`' standard domain.
#[derive(Default)]
pub struct StorageDomain;

impl StorageDomain {
    pub fn new() -> Self {
        Self
    }

    #[allow(unsafe_code)]
    fn get_dom_storage_items(request: &CdpRequest) -> CdpReply {
        let Some(storage_id) = request.params.get("storageId") else {
            return CdpReply::error("getDOMStorageItems requires storageId");
        };
        if !is_userdefaults_bucket(storage_id) {
            return CdpReply::error("unknown storage bucket");
        }
        if !is_local_storage(storage_id) {
            // No session-storage concept for NSUserDefaults — empty, not an
            // error, matching real Chrome's own answer for an origin with
            // nothing in session storage.
            return CdpReply::ok(json!({ "entries": Value::Array(vec![]) }));
        }
        // SAFETY: NSUserDefaults is documented thread-safe; no main-thread
        // requirement (see remo_objc::user_defaults's module doc).
        let entries = unsafe { remo_objc::list_user_defaults() };
        let entries: Vec<Value> = entries
            .into_iter()
            .map(|(key, value)| json!([key, display_value(&value)]))
            .collect();
        CdpReply::ok(json!({ "entries": entries }))
    }

    #[allow(unsafe_code)]
    fn set_dom_storage_item(request: &CdpRequest, events: &EventSink) -> CdpReply {
        let Some(storage_id) = request.params.get("storageId") else {
            return CdpReply::error("setDOMStorageItem requires storageId");
        };
        if !is_userdefaults_bucket(storage_id) {
            return CdpReply::error("unknown storage bucket");
        }
        let Some(key) = request.params.get("key").and_then(Value::as_str) else {
            return CdpReply::error("setDOMStorageItem requires key");
        };
        let Some(raw_value) = request.params.get("value").and_then(Value::as_str) else {
            return CdpReply::error("setDOMStorageItem requires value");
        };
        let value = parse_value(raw_value);
        // SAFETY: see get_dom_storage_items.
        if let Err(message) = unsafe { remo_objc::set_user_default(key, &value) } {
            return CdpReply::error(message);
        }
        events.emit(
            "DOMStorage.domStorageItemAdded",
            &json!({
                "storageId": storage_id_json(),
                "key": key,
                "newValue": display_value(&value),
            }),
        );
        CdpReply::empty()
    }

    #[allow(unsafe_code)]
    fn remove_dom_storage_item(request: &CdpRequest, events: &EventSink) -> CdpReply {
        let Some(storage_id) = request.params.get("storageId") else {
            return CdpReply::error("removeDOMStorageItem requires storageId");
        };
        if !is_userdefaults_bucket(storage_id) {
            return CdpReply::error("unknown storage bucket");
        }
        let Some(key) = request.params.get("key").and_then(Value::as_str) else {
            return CdpReply::error("removeDOMStorageItem requires key");
        };
        // SAFETY: see get_dom_storage_items.
        unsafe { remo_objc::delete_user_default(key) };
        events.emit(
            "DOMStorage.domStorageItemRemoved",
            &json!({
                "storageId": storage_id_json(),
                "key": key,
            }),
        );
        CdpReply::empty()
    }

    #[allow(unsafe_code)]
    fn clear(request: &CdpRequest, events: &EventSink) -> CdpReply {
        let Some(storage_id) = request.params.get("storageId") else {
            return CdpReply::error("clear requires storageId");
        };
        if !is_userdefaults_bucket(storage_id) {
            return CdpReply::error("unknown storage bucket");
        }
        // SAFETY: see get_dom_storage_items.
        let entries = unsafe { remo_objc::list_user_defaults() };
        // Deletes every key `list_user_defaults` reported. Note this can
        // never actually bring the bucket to zero entries: that list reads
        // `-dictionaryRepresentation`, which also surfaces NSGlobalDomain /
        // `registerDefaults:` keys that aren't really in this app's own
        // persistent domain, so `removeObjectForKey:` on them is a silent
        // no-op — the same thing a real Chrome "clear site data" would find
        // if it could clear NSGlobalDomain, which it can't. Not a bug in
        // this method, just the ceiling of what "clear" can mean here.
        // SAFETY: see get_dom_storage_items.
        unsafe {
            for (key, _) in &entries {
                remo_objc::delete_user_default(key);
            }
        }
        events.emit(
            "DOMStorage.domStorageItemsCleared",
            &json!({ "storageId": storage_id_json() }),
        );
        CdpReply::empty()
    }

    /// Same lookup as [`Self::get_storage_key_for_frame`], answering both
    /// `Storage.getStorageKeyForFrame` (the documented CDP method name) and
    /// `Storage.getStorageKey` (what a real Chrome 150 DevTools frontend
    /// actually sends — confirmed live via a raw-traffic capture against
    /// this exact server: `{"method":"Storage.getStorageKey","params":
    /// {"frameId":"userdefaults://standard"}}`).
    ///
    /// This was the real root cause of the Application panel's Local
    /// Storage sidebar showing "No local storage detected" despite
    /// `Page.getResourceTree` already listing the frame and raw
    /// `DOMStorage.getDOMStorageItems` calls already returning real data:
    /// only `Storage.getStorageKeyForFrame` was claimed here, so the
    /// frontend's actual `Storage.getStorageKey` call fell through to the
    /// transport's generic `{}` ack (see `transport.rs`'s `None` arm) —
    /// `frame.getStorageKey()` in `devtools-frontend`'s `ResourceTreeModel`
    /// then resolved to `undefined` (no `storageKey` field in the reply),
    /// so `StorageKeyManager` never learned this frame's key and
    /// `DOMStorageModel` never created a `DOMStorage` entry for it — all
    /// upstream of (and invisible to) any raw `DOMStorage.*` call, which is
    /// exactly why scripting the wire protocol directly didn't catch this.
    fn get_storage_key(request: &CdpRequest) -> CdpReply {
        Self::get_storage_key_for_frame(request)
    }

    fn get_storage_key_for_frame(request: &CdpRequest) -> CdpReply {
        // Frame ids: "1" is the main document (`domain_dom::MAIN_ORIGIN`),
        // anything else is an origin string handed out verbatim as the
        // child frame's own id (see domain_dom.rs's Page.getResourceTree).
        let frame_id = request.params.get("frameId").and_then(Value::as_str);
        let origin = match frame_id {
            Some(id) if id == USERDEFAULTS_ORIGIN => USERDEFAULTS_ORIGIN,
            _ => crate::domain_dom::MAIN_ORIGIN,
        };
        CdpReply::ok(json!({ "storageKey": storage_key(origin) }))
    }
}

#[async_trait]
impl CdpDomain for StorageDomain {
    fn methods(&self) -> &'static [&'static str] {
        &[
            "DOMStorage.enable",
            "DOMStorage.disable",
            "DOMStorage.getDOMStorageItems",
            "DOMStorage.setDOMStorageItem",
            "DOMStorage.removeDOMStorageItem",
            "DOMStorage.clear",
            "Storage.getStorageKeyForFrame",
            "Storage.getStorageKey",
            "Storage.setStorageBucketTracking",
            "Storage.clearDataForOrigin",
            "Storage.clearDataForStorageKey",
        ]
    }

    async fn respond(&self, request: &CdpRequest, events: &EventSink) -> CdpReply {
        match request.method.as_str() {
            "DOMStorage.enable" | "DOMStorage.disable" => CdpReply::empty(),
            "DOMStorage.getDOMStorageItems" => Self::get_dom_storage_items(request),
            "DOMStorage.setDOMStorageItem" => Self::set_dom_storage_item(request, events),
            "DOMStorage.removeDOMStorageItem" => Self::remove_dom_storage_item(request, events),
            "DOMStorage.clear" => Self::clear(request, events),
            "Storage.getStorageKeyForFrame" => Self::get_storage_key_for_frame(request),
            "Storage.getStorageKey" => Self::get_storage_key(request),
            // Acked, deliberately without a `Storage.storageBucketCreatedOrUpdated`
            // event — see this module's doc comment on the IndexedDB/CacheStorage
            // non-goal.
            "Storage.setStorageBucketTracking" => CdpReply::empty(),
            // "Clear site data" would mean wiping the one bucket we expose —
            // fail loudly rather than silently doing nothing, matching the
            // Swift reference.
            "Storage.clearDataForOrigin" | "Storage.clearDataForStorageKey" => {
                CdpReply::error("not supported — clear the storage bucket instead")
            }
            other => CdpReply::error(format!("Storage domain does not handle {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn assert_err(reply: CdpReply) -> String {
        match reply {
            CdpReply::Result(value) => panic!("expected Error reply, got result: {value}"),
            CdpReply::Error { message, .. } => message,
        }
    }

    fn userdefaults_storage_id() -> Value {
        json!({
            "securityOrigin": USERDEFAULTS_ORIGIN,
            "storageKey": format!("{USERDEFAULTS_ORIGIN}/"),
            "isLocalStorage": true,
        })
    }

    #[tokio::test]
    async fn enable_and_disable_are_bare_acks() {
        let domain = StorageDomain::new();
        let (events, _rx) = EventSink::new();
        for method in ["DOMStorage.enable", "DOMStorage.disable"] {
            let reply = domain.respond(&request(method, json!({})), &events).await;
            assert_eq!(assert_ok(reply), json!({}));
        }
    }

    #[tokio::test]
    async fn get_dom_storage_items_rejects_unknown_bucket() {
        let domain = StorageDomain::new();
        let (events, _rx) = EventSink::new();
        let reply = domain
            .respond(
                &request(
                    "DOMStorage.getDOMStorageItems",
                    json!({ "storageId": { "securityOrigin": "mmkv://whatever", "isLocalStorage": true } }),
                ),
                &events,
            )
            .await;
        assert_err(reply);
    }

    #[tokio::test]
    async fn get_dom_storage_items_returns_empty_entries_for_session_storage() {
        let domain = StorageDomain::new();
        let (events, _rx) = EventSink::new();
        let mut storage_id = userdefaults_storage_id();
        storage_id["isLocalStorage"] = json!(false);
        let reply = domain
            .respond(
                &request(
                    "DOMStorage.getDOMStorageItems",
                    json!({ "storageId": storage_id }),
                ),
                &events,
            )
            .await;
        assert_eq!(assert_ok(reply), json!({ "entries": [] }));
    }

    #[tokio::test]
    async fn get_dom_storage_items_accepts_security_origin_fallback() {
        let domain = StorageDomain::new();
        let (events, _rx) = EventSink::new();
        let reply = domain
            .respond(
                &request(
                    "DOMStorage.getDOMStorageItems",
                    json!({ "storageId": { "securityOrigin": USERDEFAULTS_ORIGIN, "isLocalStorage": true } }),
                ),
                &events,
            )
            .await;
        let value = assert_ok(reply);
        assert!(value.get("entries").unwrap().is_array());
    }

    #[cfg(target_vendor = "apple")]
    #[tokio::test]
    async fn set_then_get_round_trips_a_string() {
        let domain = StorageDomain::new();
        let (events, mut rx) = EventSink::new();
        let key = "remo.cdp-tests.set_then_get_round_trips_a_string";
        let set_reply = domain
            .respond(
                &request(
                    "DOMStorage.setDOMStorageItem",
                    json!({ "storageId": userdefaults_storage_id(), "key": key, "value": "hello" }),
                ),
                &events,
            )
            .await;
        assert_eq!(assert_ok(set_reply), json!({}));

        let event = rx.try_recv().expect("expected domStorageItemAdded event");
        assert_eq!(event["method"], "DOMStorage.domStorageItemAdded");
        assert_eq!(event["params"]["key"], key);
        assert_eq!(event["params"]["newValue"], "hello");

        let get_reply = domain
            .respond(
                &request(
                    "DOMStorage.getDOMStorageItems",
                    json!({ "storageId": userdefaults_storage_id() }),
                ),
                &events,
            )
            .await;
        let entries = assert_ok(get_reply)["entries"].clone();
        let entries = entries.as_array().unwrap();
        assert!(entries.iter().any(|e| e == &json!([key, "hello"])));

        // Clean up.
        let _ = domain
            .respond(
                &request(
                    "DOMStorage.removeDOMStorageItem",
                    json!({ "storageId": userdefaults_storage_id(), "key": key }),
                ),
                &events,
            )
            .await;
    }

    #[cfg(target_vendor = "apple")]
    #[tokio::test]
    async fn non_string_values_round_trip_json_encoded() {
        let domain = StorageDomain::new();
        let (events, _rx) = EventSink::new();
        let key = "remo.cdp-tests.non_string_values_round_trip_json_encoded";
        let _ = domain
            .respond(
                &request(
                    "DOMStorage.setDOMStorageItem",
                    json!({ "storageId": userdefaults_storage_id(), "key": key, "value": "42" }),
                ),
                &events,
            )
            .await;
        let get_reply = domain
            .respond(
                &request(
                    "DOMStorage.getDOMStorageItems",
                    json!({ "storageId": userdefaults_storage_id() }),
                ),
                &events,
            )
            .await;
        let entries = assert_ok(get_reply)["entries"].clone();
        let entries = entries.as_array().unwrap();
        // Written as the string "42", parsed as JSON number 42, displayed
        // back as the text "42" — round-trips as a number under the hood,
        // reads the same in the table either way.
        assert!(entries.iter().any(|e| e == &json!([key, "42"])));

        let _ = domain
            .respond(
                &request(
                    "DOMStorage.removeDOMStorageItem",
                    json!({ "storageId": userdefaults_storage_id(), "key": key }),
                ),
                &events,
            )
            .await;
    }

    #[cfg(target_vendor = "apple")]
    #[tokio::test]
    async fn remove_dom_storage_item_deletes_and_emits_event() {
        let domain = StorageDomain::new();
        let (events, mut rx) = EventSink::new();
        let key = "remo.cdp-tests.remove_dom_storage_item_deletes_and_emits_event";
        let _ = domain
            .respond(
                &request(
                    "DOMStorage.setDOMStorageItem",
                    json!({ "storageId": userdefaults_storage_id(), "key": key, "value": "x" }),
                ),
                &events,
            )
            .await;
        let _ = rx.try_recv(); // drain the "added" event

        let reply = domain
            .respond(
                &request(
                    "DOMStorage.removeDOMStorageItem",
                    json!({ "storageId": userdefaults_storage_id(), "key": key }),
                ),
                &events,
            )
            .await;
        assert_eq!(assert_ok(reply), json!({}));
        let event = rx.try_recv().expect("expected domStorageItemRemoved event");
        assert_eq!(event["method"], "DOMStorage.domStorageItemRemoved");
        assert_eq!(event["params"]["key"], key);

        let get_reply = domain
            .respond(
                &request(
                    "DOMStorage.getDOMStorageItems",
                    json!({ "storageId": userdefaults_storage_id() }),
                ),
                &events,
            )
            .await;
        let entries = assert_ok(get_reply)["entries"].clone();
        let entries = entries.as_array().unwrap();
        assert!(!entries.iter().any(|e| e[0] == key));
    }

    #[cfg(target_vendor = "apple")]
    #[tokio::test]
    async fn clear_removes_everything_and_emits_event() {
        let domain = StorageDomain::new();
        let (events, mut rx) = EventSink::new();
        let key = "remo.cdp-tests.clear_removes_everything_and_emits_event";
        let _ = domain
            .respond(
                &request(
                    "DOMStorage.setDOMStorageItem",
                    json!({ "storageId": userdefaults_storage_id(), "key": key, "value": "x" }),
                ),
                &events,
            )
            .await;
        let _ = rx.try_recv(); // drain the "added" event

        let reply = domain
            .respond(
                &request(
                    "DOMStorage.clear",
                    json!({ "storageId": userdefaults_storage_id() }),
                ),
                &events,
            )
            .await;
        assert_eq!(assert_ok(reply), json!({}));
        let event = rx
            .try_recv()
            .expect("expected domStorageItemsCleared event");
        assert_eq!(event["method"], "DOMStorage.domStorageItemsCleared");

        let get_reply = domain
            .respond(
                &request(
                    "DOMStorage.getDOMStorageItems",
                    json!({ "storageId": userdefaults_storage_id() }),
                ),
                &events,
            )
            .await;
        let entries = assert_ok(get_reply)["entries"].clone();
        // Not asserting the list is *empty*: `list_user_defaults` reads
        // `-dictionaryRepresentation`, which also surfaces NSGlobalDomain /
        // `registerDefaults:` keys that `removeObjectForKey:` cannot
        // actually remove (they're not really in this process's own
        // persistent domain) — a real clear of a real app's defaults still
        // leaves those behind. What matters is that our own test key, which
        // *is* in the persistent domain, is gone.
        assert!(!entries.as_array().unwrap().iter().any(|e| e[0] == key));
    }

    #[tokio::test]
    async fn get_storage_key_for_frame_maps_main_and_userdefaults_frames() {
        let domain = StorageDomain::new();
        let (events, _rx) = EventSink::new();

        let main_reply = domain
            .respond(
                &request("Storage.getStorageKeyForFrame", json!({ "frameId": "1" })),
                &events,
            )
            .await;
        assert_eq!(
            assert_ok(main_reply)["storageKey"],
            format!("{}/", crate::domain_dom::MAIN_ORIGIN)
        );

        let ud_reply = domain
            .respond(
                &request(
                    "Storage.getStorageKeyForFrame",
                    json!({ "frameId": USERDEFAULTS_ORIGIN }),
                ),
                &events,
            )
            .await;
        assert_eq!(
            assert_ok(ud_reply)["storageKey"],
            format!("{USERDEFAULTS_ORIGIN}/")
        );
    }

    #[tokio::test]
    async fn set_storage_bucket_tracking_is_a_bare_ack() {
        let domain = StorageDomain::new();
        let (events, mut rx) = EventSink::new();
        let reply = domain
            .respond(
                &request(
                    "Storage.setStorageBucketTracking",
                    json!({ "storageKey": format!("{}/", crate::domain_dom::MAIN_ORIGIN) }),
                ),
                &events,
            )
            .await;
        assert_eq!(assert_ok(reply), json!({}));
        assert!(
            rx.try_recv().is_err(),
            "must not emit storageBucketCreatedOrUpdated until IndexedDB/CacheStorage exist"
        );
    }

    #[tokio::test]
    async fn clear_data_for_origin_is_unsupported() {
        let domain = StorageDomain::new();
        let (events, _rx) = EventSink::new();
        let reply = domain
            .respond(&request("Storage.clearDataForOrigin", json!({})), &events)
            .await;
        assert_err(reply);
    }

    #[tokio::test]
    async fn methods_list_matches_exactly_what_this_domain_claims() {
        let domain = StorageDomain::new();
        assert_eq!(
            domain.methods(),
            &[
                "DOMStorage.enable",
                "DOMStorage.disable",
                "DOMStorage.getDOMStorageItems",
                "DOMStorage.setDOMStorageItem",
                "DOMStorage.removeDOMStorageItem",
                "DOMStorage.clear",
                "Storage.getStorageKeyForFrame",
                "Storage.getStorageKey",
                "Storage.setStorageBucketTracking",
                "Storage.clearDataForOrigin",
                "Storage.clearDataForStorageKey",
            ]
        );
    }

    #[tokio::test]
    async fn get_storage_key_answers_the_same_as_get_storage_key_for_frame() {
        // Regression test for the real root cause of the Application
        // panel's Local Storage sidebar bug: a real Chrome 150 frontend
        // sends `Storage.getStorageKey`, not `Storage.getStorageKeyForFrame`
        // — confirmed via a raw-traffic capture against this exact server.
        // Before this method was claimed here, it fell through to the
        // transport's generic `{}` ack, which has no `storageKey` field, so
        // `ResourceTreeFrame.getStorageKey()` resolved to nothing and the
        // frame's storage key never reached `StorageKeyManager`.
        let domain = StorageDomain::new();
        let (events, _rx) = EventSink::new();

        let main_reply = domain
            .respond(
                &request("Storage.getStorageKey", json!({ "frameId": "1" })),
                &events,
            )
            .await;
        assert_eq!(
            assert_ok(main_reply)["storageKey"],
            format!("{}/", crate::domain_dom::MAIN_ORIGIN)
        );

        let ud_reply = domain
            .respond(
                &request(
                    "Storage.getStorageKey",
                    json!({ "frameId": USERDEFAULTS_ORIGIN }),
                ),
                &events,
            )
            .await;
        assert_eq!(
            assert_ok(ud_reply)["storageKey"],
            format!("{USERDEFAULTS_ORIGIN}/")
        );
    }
}
