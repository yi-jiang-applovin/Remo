//! Integration tests: spin up the real, embedded `remo-sdk` `RemoServer` on
//! localhost and drive it as a genuine CDP client would — HTTP discovery,
//! then a WebSocket dial to `/devtools/page/1`, then `Remo.invoke`/
//! `Remo.listCapabilities` calls — using `tokio-tungstenite` exactly the way
//! real Chrome DevTools or an agent script would.
//!
//! Phase 3 of the CDP rewrite plan cut the legacy length-prefixed protocol
//! (and its clients, `remo-desktop`/`remo-daemon`) out of `RemoServer`
//! entirely, so these tests no longer have a "legacy client" counterpart to
//! exercise — this file used to. Two capabilities documented as known,
//! tracked gaps rather than silently dropped:
//!
//! - `Remo.capabilitiesChanged` isn't wired up on the server side yet
//!   (`RemoDomain::respond` doesn't forward the registry's event bus through
//!   its `EventSink`), so there's no test here proving a CDP client observes
//!   a live capability registration — the old
//!   `capabilities_changed_event_on_{register,unregister}` tests relied on
//!   the legacy wire's event forwarding, which no longer exists.
//! - The high-fidelity H.264 mirror (`__start_mirror`/`__stop_mirror`) has
//!   no CDP equivalent yet — see `remo-sdk/src/streaming.rs`'s module doc for
//!   the planned `Remo.startMirror`/`Remo.stopMirror` extension. The old
//!   `start_and_stop_mirror`/`start_mirror_twice_returns_error` tests are
//!   gone with the RPC surface they tested.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use remo_sdk::{CapabilityRegistry, RemoServer};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, WebSocketStream};

/// A thin CDP test client: dial the target's devtools WebSocket once, then
/// send/await request-reply pairs matched by `id`. Deliberately minimal —
/// this is a test harness, not `remo-cli`'s own client (see
/// `crates/remo-cli/src/cdp_client.rs` for the real one).
struct TestCdpClient {
    ws: WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    next_id: u64,
}

impl TestCdpClient {
    async fn connect(port: u16) -> Self {
        let (ws, _response) = connect_async(format!("ws://127.0.0.1:{port}/devtools/page/1"))
            .await
            .expect("CDP client should be able to open a WebSocket to the real RemoServer");
        Self { ws, next_id: 1 }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;

        self.ws
            .send(WsMessage::Text(
                json!({ "id": id, "method": method, "params": params })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();

        let reply = tokio::time::timeout(Duration::from_secs(5), self.ws.next())
            .await
            .expect("should receive a CDP reply within timeout")
            .expect("stream should not end")
            .expect("should be a valid WS message");

        let envelope: Value = match reply {
            WsMessage::Text(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("expected a text frame, got {other:?}"),
        };
        assert_eq!(envelope["id"], id);
        envelope
    }

    /// Calls `Remo.invoke` and unwraps its `{"result": <capability's JSON>}`
    /// layer (see `remo-cdp/src/domain_remo.rs::invoke`) — panics with the
    /// CDP error message if the call failed.
    async fn invoke(&mut self, name: &str, args: Value) -> Value {
        let envelope = self
            .call("Remo.invoke", json!({ "name": name, "args": args }))
            .await;
        if let Some(error) = envelope.get("error") {
            panic!("Remo.invoke({name}) failed: {error}");
        }
        envelope["result"]["result"].clone()
    }
}

#[tokio::test]
async fn full_roundtrip_over_cdp() {
    let registry = CapabilityRegistry::new();
    registry.register_sync("echo", |params| Ok(json!({ "echoed": params })));
    registry.register_sync("add", |params| {
        let a = params["a"].as_i64().unwrap_or(0);
        let b = params["b"].as_i64().unwrap_or(0);
        Ok(json!({ "sum": a + b }))
    });

    let server = RemoServer::new(registry, 0);
    let shutdown = server.shutdown_handle();
    let (port_tx, port_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move {
        server.run(Some(port_tx)).await.unwrap();
    });
    let port = tokio::time::timeout(Duration::from_secs(2), port_rx)
        .await
        .expect("server did not report port in time")
        .expect("port sender dropped");

    let mut client = TestCdpClient::connect(port).await;

    assert_eq!(
        client.invoke("echo", json!({"hello": "world"})).await,
        json!({"echoed": {"hello": "world"}})
    );
    assert_eq!(
        client.invoke("add", json!({"a": 17, "b": 25})).await,
        json!({"sum": 42})
    );
    assert_eq!(
        client.invoke("__ping", json!({})).await,
        json!({"pong": true})
    );

    let names_envelope = client.call("Remo.listCapabilities", json!({})).await;
    let names: Vec<&str> = names_envelope["result"]["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"add"));
    assert!(names.contains(&"__ping"));

    let error_envelope = client
        .call(
            "Remo.invoke",
            json!({ "name": "no_such_thing", "args": {} }),
        )
        .await;
    assert!(
        error_envelope.get("error").is_some(),
        "expected an error for an unknown capability, got: {error_envelope}"
    );

    shutdown.send(()).ok();
    server_handle.abort();
}

/// Proves the real, embedded `RemoServer` — not the standalone `remo-cdp`
/// example — serves HTTP discovery and a genuine CDP WebSocket client
/// end to end, dialing exactly the path Chrome DevTools itself would.
#[tokio::test]
async fn discovery_and_websocket_are_both_reachable_on_the_real_server() {
    let registry = CapabilityRegistry::new();
    let server = RemoServer::new(registry, 0);
    let shutdown = server.shutdown_handle();
    let (port_tx, port_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move {
        server.run(Some(port_tx)).await.unwrap();
    });
    let port = tokio::time::timeout(Duration::from_secs(2), port_rx)
        .await
        .expect("server did not report port in time")
        .expect("port sender dropped");

    let discovery = http_get_json(port, "/json/version").await;
    assert!(
        discovery["remoProtocolVersion"].is_string(),
        "discovery response: {discovery}"
    );

    let mut client = TestCdpClient::connect(port).await;
    let reply = client.call("Remo.listCapabilities", json!({})).await;
    assert!(reply["result"]["names"].is_array());

    shutdown.send(()).ok();
    server_handle.abort();
}

/// Proves the generic storage-debugging built-ins (`userDefaults.*`,
/// `filesystem.*`, `sqlite.query`) are reachable end to end through the real
/// server — registered by `RemoServer::new` itself (see
/// `remo-sdk/src/server.rs::register_storage_debugging`), not something a
/// test has to register — via a real CDP `Remo.invoke` round trip, the same
/// path `remo-cli`/`remo-mcp`/the Console panel all use.
#[tokio::test]
async fn storage_debugging_capabilities_are_reachable_over_cdp() {
    let registry = CapabilityRegistry::new();
    let server = RemoServer::new(registry, 0);
    let shutdown = server.shutdown_handle();
    let (port_tx, port_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move {
        server.run(Some(port_tx)).await.unwrap();
    });
    let port = tokio::time::timeout(Duration::from_secs(2), port_rx)
        .await
        .expect("server did not report port in time")
        .expect("port sender dropped");

    let mut client = TestCdpClient::connect(port).await;

    // userDefaults.* — a real NSUserDefaults key on whatever machine runs
    // this test, scoped under a test-only prefix so it never collides with
    // real app state.
    let key = format!("remo.integration-test.{}", std::process::id());
    let set_result = client
        .invoke("userDefaults.set", json!({"key": key, "value": "hello"}))
        .await;
    assert_eq!(set_result["value"], "hello");

    let get_result = client.invoke("userDefaults.get", json!({"key": key})).await;
    assert_eq!(get_result["value"], "hello");

    let list_result = client.invoke("userDefaults.list", json!({})).await;
    assert_eq!(list_result[&key], "hello");

    let delete_result = client
        .invoke("userDefaults.delete", json!({"key": key}))
        .await;
    assert_eq!(delete_result["deleted"], true);

    let get_after_delete = client.invoke("userDefaults.get", json!({"key": key})).await;
    assert_eq!(get_after_delete["value"], Value::Null);

    // filesystem.* — a real temp file/directory.
    let dir = std::env::temp_dir().join(format!("remo-integration-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hello.txt"), b"hello world").unwrap();

    let list_dir = client
        .invoke("filesystem.list", json!({"path": dir.to_str().unwrap()}))
        .await;
    let entries = list_dir.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "hello.txt");

    let read_result = client
        .invoke(
            "filesystem.read",
            json!({"path": dir.join("hello.txt").to_str().unwrap()}),
        )
        .await;
    assert_eq!(read_result["size"], 11);

    let delete_file_result = client
        .invoke("filesystem.delete", json!({"path": dir.to_str().unwrap()}))
        .await;
    assert_eq!(delete_file_result["deleted"], true);
    assert!(!dir.exists());

    // sqlite.query — a real temp SQLite database.
    let db_path = std::env::temp_dir().join(format!(
        "remo-integration-test-{}.sqlite",
        std::process::id()
    ));
    let db_path_str = db_path.to_str().unwrap();

    let create = client
        .invoke(
            "sqlite.query",
            json!({"path": db_path_str, "sql": "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)"}),
        )
        .await;
    assert_eq!(create["rows_affected"], 0);

    let insert = client
        .invoke(
            "sqlite.query",
            json!({"path": db_path_str, "sql": "INSERT INTO items (name) VALUES ('widget')"}),
        )
        .await;
    assert_eq!(insert["rows_affected"], 1);

    let select = client
        .invoke(
            "sqlite.query",
            json!({"path": db_path_str, "sql": "SELECT id, name FROM items"}),
        )
        .await;
    assert_eq!(select["columns"], json!(["id", "name"]));
    assert_eq!(select["rows"], json!([[1, "widget"]]));

    std::fs::remove_file(&db_path).ok();

    shutdown.send(()).ok();
    server_handle.abort();
}

/// A tiny hand-rolled HTTP/1.0 GET, avoiding a full HTTP client crate just
/// for this one discovery-endpoint assertion (mirrors the technique already
/// used in `remo-cdp`'s own `dual_stack` tests).
async fn http_get_json(port: u16, path: &str) -> Value {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let host = format!("127.0.0.1:{port}");
    let mut stream = tokio::net::TcpStream::connect(&host).await.unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.0\r\nHost: {host}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let body = response.split("\r\n\r\n").nth(1).unwrap_or_default();
    serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("bad JSON body: {e}\nresponse: {response}"))
}
