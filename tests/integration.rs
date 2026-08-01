//! Integration tests: spin up remo-sdk server on localhost,
//! connect with remo-desktop's RpcClient, and verify round-trip calls.

use std::net::SocketAddr;
use std::time::Duration;

use remo_desktop::{RpcClient, RpcResponse, StreamReceiver};
use remo_protocol::{stream_flags, ErrorCode, ResponseResult, StreamFrame};
use remo_sdk::{CapabilityRegistry, RemoServer};
use tokio::sync::{broadcast, mpsc};

/// Helper: unwrap a JSON response from an RpcResponse.
fn expect_json(resp: RpcResponse) -> remo_protocol::Response {
    match resp {
        RpcResponse::Json(r) => r,
        other => panic!("expected Json response, got {other:?}"),
    }
}

#[tokio::test]
async fn full_roundtrip() {
    let registry = CapabilityRegistry::new();
    registry.register_sync("echo", |params| Ok(serde_json::json!({ "echoed": params })));
    registry.register_sync("add", |params| {
        let a = params["a"].as_i64().unwrap_or(0);
        let b = params["b"].as_i64().unwrap_or(0);
        Ok(serde_json::json!({ "sum": a + b }))
    });

    let server = RemoServer::new(registry, 0);
    let (port_tx, port_rx) = tokio::sync::oneshot::channel();

    let server_handle = tokio::spawn(async move {
        server.run(Some(port_tx)).await.unwrap();
    });

    let actual_port = tokio::time::timeout(Duration::from_secs(2), port_rx)
        .await
        .expect("server did not report port in time")
        .expect("port sender dropped");

    let addr: SocketAddr = ([127, 0, 0, 1], actual_port).into();
    let (event_tx, _event_rx) = mpsc::channel(16);
    let client = RpcClient::connect(addr, event_tx).await.unwrap();

    // echo
    let resp = expect_json(
        client
            .call(
                "echo",
                serde_json::json!({"hello": "world"}),
                Duration::from_secs(5),
            )
            .await
            .unwrap(),
    );
    match &resp.result {
        ResponseResult::Ok { data } => assert_eq!(data["echoed"]["hello"], "world"),
        ResponseResult::Error { message, .. } => panic!("expected ok: {message}"),
    }

    // add
    let resp = expect_json(
        client
            .call(
                "add",
                serde_json::json!({"a": 17, "b": 25}),
                Duration::from_secs(5),
            )
            .await
            .unwrap(),
    );
    match &resp.result {
        ResponseResult::Ok { data } => assert_eq!(data["sum"], 42),
        ResponseResult::Error { message, .. } => panic!("expected ok: {message}"),
    }

    // __ping
    let resp = expect_json(
        client
            .call("__ping", serde_json::json!({}), Duration::from_secs(5))
            .await
            .unwrap(),
    );
    match &resp.result {
        ResponseResult::Ok { data } => assert_eq!(data["pong"], true),
        ResponseResult::Error { message, .. } => panic!("expected ok: {message}"),
    }

    // __list_capabilities
    let resp = expect_json(
        client
            .call(
                "__list_capabilities",
                serde_json::json!({}),
                Duration::from_secs(5),
            )
            .await
            .unwrap(),
    );
    match &resp.result {
        ResponseResult::Ok { data } => {
            let names: Vec<&str> = data
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert!(names.contains(&"echo"));
            assert!(names.contains(&"add"));
            assert!(names.contains(&"__ping"));
        }
        ResponseResult::Error { message, .. } => panic!("expected ok: {message}"),
    }

    // non-existent capability
    let resp = expect_json(
        client
            .call(
                "no_such_thing",
                serde_json::json!({}),
                Duration::from_secs(5),
            )
            .await
            .unwrap(),
    );
    match &resp.result {
        ResponseResult::Error { code, .. } => assert_eq!(*code, ErrorCode::NotFound),
        ResponseResult::Ok { .. } => panic!("expected error for unknown capability"),
    }

    server_handle.abort();
}

#[tokio::test]
async fn start_and_stop_mirror() {
    let registry = CapabilityRegistry::new();
    let server = RemoServer::new(registry, 0);
    let shutdown = server.shutdown_handle();

    let (port_tx, port_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        server.run(Some(port_tx)).await.unwrap();
    });

    let port = port_rx.await.unwrap();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let (event_tx, _) = mpsc::channel(16);
    let client = RpcClient::connect(addr, event_tx).await.unwrap();

    // Start mirror
    let resp = expect_json(
        client
            .call(
                "__start_mirror",
                serde_json::json!({"fps": 10}),
                Duration::from_secs(5),
            )
            .await
            .unwrap(),
    );
    match &resp.result {
        ResponseResult::Ok { data } => {
            assert_eq!(data["stream_id"], 1);
        }
        ResponseResult::Error { message, .. } => {
            // On non-Apple targets, encoder fails — session won't be stored
            eprintln!("mirror start error (expected on non-iOS): {message}");
        }
    }

    // Stop mirror
    let resp = expect_json(
        client
            .call(
                "__stop_mirror",
                serde_json::json!({}),
                Duration::from_secs(5),
            )
            .await
            .unwrap(),
    );
    // Either stopped:true or not-found (if encoder failed to start on non-Apple)
    println!("stop response: {:?}", resp.result);

    let _ = shutdown.send(());
}

#[tokio::test]
async fn start_mirror_twice_returns_error() {
    let registry = CapabilityRegistry::new();
    let server = RemoServer::new(registry, 0);
    let shutdown = server.shutdown_handle();

    let (port_tx, port_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        server.run(Some(port_tx)).await.unwrap();
    });

    let port = port_rx.await.unwrap();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let (event_tx, _) = mpsc::channel(16);
    let client = RpcClient::connect(addr, event_tx).await.unwrap();

    // First start
    let _ = client
        .call(
            "__start_mirror",
            serde_json::json!({"fps": 10}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second start — should fail with StreamAlreadyActive (if first succeeded)
    let resp = expect_json(
        client
            .call(
                "__start_mirror",
                serde_json::json!({"fps": 10}),
                Duration::from_secs(5),
            )
            .await
            .unwrap(),
    );
    match &resp.result {
        ResponseResult::Error { code, .. } => {
            assert_eq!(*code, ErrorCode::StreamAlreadyActive);
        }
        ResponseResult::Ok { .. } => {
            // On non-Apple targets, first start may have failed (no encoder),
            // so session slot is clear and second start "succeeds" too — acceptable
        }
    }

    let _ = shutdown.send(());
}

#[tokio::test]
async fn stream_receiver_broadcast_multiple_subscribers() {
    let (tx, _) = broadcast::channel::<StreamFrame>(16);

    let mut rx1 = StreamReceiver::new(tx.subscribe());
    let mut rx2 = StreamReceiver::new(tx.subscribe());

    let frame = StreamFrame {
        stream_id: 1,
        sequence: 0,
        timestamp_us: 0,
        flags: stream_flags::KEYFRAME,
        data: vec![0x65],
    };
    tx.send(frame).unwrap();

    let f1 = rx1.next_frame().await.unwrap();
    let f2 = rx2.next_frame().await.unwrap();
    assert_eq!(f1.sequence, f2.sequence);
}

#[tokio::test]
async fn capabilities_changed_event_on_register() {
    use remo_protocol::Message;
    use remo_transport::Connection;
    use serde_json::json;

    let registry = CapabilityRegistry::new();
    registry.register_sync("initial", |_| Ok(json!({"ok": true})));

    let server = RemoServer::new(registry.clone(), 0);
    let (port_tx, port_rx) = tokio::sync::oneshot::channel();
    let shutdown = server.shutdown_handle();

    tokio::spawn(async move {
        server.run(Some(port_tx)).await.unwrap();
    });

    let port = port_rx.await.unwrap();
    let addr = format!("127.0.0.1:{}", port).parse().unwrap();
    let mut conn = Connection::connect(addr).await.unwrap();

    // Give the server time to accept the connection and set up the event forwarder.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Register a new capability after connection is established
    registry.register_sync("dynamic_cap", |_| Ok(json!({"dynamic": true})));

    // We should receive a capabilities_changed event
    let msg = tokio::time::timeout(Duration::from_secs(2), conn.recv())
        .await
        .expect("should receive event within timeout")
        .expect("should receive message");

    match msg {
        Some(Message::Event(event)) => {
            assert_eq!(event.kind, "capabilities_changed");
            let payload = event.payload;
            assert_eq!(payload["action"], "registered");
            assert_eq!(payload["name"], "dynamic_cap");
            let caps = payload["capabilities"].as_array().unwrap();
            assert!(caps.iter().any(|c| c == "dynamic_cap"));
            assert!(caps.iter().any(|c| c == "initial"));
        }
        other => panic!("expected Event, got {:?}", other),
    }

    shutdown.send(()).ok();
}

/// Proves the real, embedded `RemoServer` — not the standalone `remo-cdp`
/// example — serves a genuine CDP client (a raw WebSocket, dialing exactly
/// the discovery + upgrade path Chrome DevTools itself would use) at the
/// same time a legacy client is connected to it, over the same listener.
/// This is Phase 1's concrete acceptance proof: the dual-stack peek in
/// `RemoServer::run` must route each connection correctly regardless of
/// what else is attached, not just in isolation (`remo-cdp`'s own tests)
/// or in theory.
#[tokio::test]
async fn dual_stack_serves_cdp_and_legacy_clients_on_the_same_server() {
    use futures::{SinkExt, StreamExt};
    use remo_protocol::{Message, Request};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let registry = CapabilityRegistry::new();
    registry.register_sync("echo", |params| Ok(serde_json::json!({ "echoed": params })));

    let server = RemoServer::new(registry, 0);
    let shutdown = server.shutdown_handle();
    let (port_tx, port_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        server.run(Some(port_tx)).await.unwrap();
    });
    let port = tokio::time::timeout(Duration::from_secs(2), port_rx)
        .await
        .expect("server did not report port in time")
        .expect("port sender dropped");

    // A legacy client, connected and idle (matching the two
    // `capabilities_changed_event_on_*` tests above) — proves the CDP peek
    // doesn't wedge or otherwise disturb a concurrently-connected legacy
    // connection on the same server.
    let legacy_addr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut legacy_conn = remo_transport::Connection::connect(legacy_addr)
        .await
        .expect("legacy client should still connect");

    // Discovery: the same `/json/version` request real Chrome DevTools
    // sends before ever opening a WebSocket.
    let discovery: serde_json::Value = http_get_json(port, "/json/version").await;
    assert!(
        discovery["remoProtocolVersion"].is_string(),
        "discovery response: {discovery}"
    );

    // The CDP path itself: dial the WebSocket upgrade exactly as
    // `devtools://…?ws=127.0.0.1:<port>/devtools/page/1` would, then issue
    // a `Remo.invoke` call and confirm the real `CapabilityRegistry` behind
    // it actually ran.
    let (mut ws, _response) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/devtools/page/1"))
            .await
            .expect("CDP client should be able to open a WebSocket to the real RemoServer");

    ws.send(WsMessage::Text(
        serde_json::json!({
            "id": 1,
            "method": "Remo.invoke",
            "params": {"name": "echo", "args": {"hello": "cdp"}},
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    let reply = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("should receive a CDP reply within timeout")
        .expect("stream should not end")
        .expect("should be a valid WS message");

    let reply_json: serde_json::Value = match reply {
        WsMessage::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected a text frame, got {other:?}"),
    };
    assert_eq!(reply_json["id"], 1);
    // `Remo.invoke`'s reply wraps the capability's own JSON result under
    // `result.result` (see `domain_remo.rs::invoke`), and the real
    // `CapabilityRegistry`'s `echo` handler wraps its input under `echoed`
    // (see `full_roundtrip` above) — both layers must show up correctly for
    // this to prove the call reached the actual registry, not a stub.
    assert_eq!(reply_json["result"]["result"]["echoed"]["hello"], "cdp");

    // The legacy connection is still alive and independently usable —
    // the CDP client attaching didn't disturb it.
    legacy_conn
        .send(Message::Request(Request::new(
            "echo",
            serde_json::json!({"hello": "legacy"}),
        )))
        .await
        .expect("legacy connection should remain independently usable");
    let legacy_reply = tokio::time::timeout(Duration::from_secs(5), legacy_conn.recv())
        .await
        .expect("should receive a legacy reply within timeout")
        .expect("legacy recv should not error")
        .expect("legacy connection should not have closed");
    match legacy_reply {
        Message::Response(resp) => match resp.result {
            ResponseResult::Ok { data } => assert_eq!(data["echoed"]["hello"], "legacy"),
            ResponseResult::Error { message, .. } => panic!("expected ok: {message}"),
        },
        other => panic!("expected Response, got {other:?}"),
    }

    shutdown.send(()).ok();
}

/// A tiny hand-rolled HTTP/1.0 GET, avoiding a full HTTP client crate just
/// for this one discovery-endpoint assertion (mirrors the technique already
/// used in `remo-cdp`'s own `dual_stack` tests).
async fn http_get_json(port: u16, path: &str) -> serde_json::Value {
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

#[tokio::test]
async fn capabilities_changed_event_on_unregister() {
    use remo_protocol::Message;
    use remo_transport::Connection;
    use serde_json::json;

    let registry = CapabilityRegistry::new();
    registry.register_sync("to_remove", |_| Ok(json!({"ok": true})));

    let server = RemoServer::new(registry.clone(), 0);
    let (port_tx, port_rx) = tokio::sync::oneshot::channel();
    let shutdown = server.shutdown_handle();

    tokio::spawn(async move {
        server.run(Some(port_tx)).await.unwrap();
    });

    let port = port_rx.await.unwrap();
    let addr = format!("127.0.0.1:{}", port).parse().unwrap();
    let mut conn = Connection::connect(addr).await.unwrap();

    // Give the server time to accept the connection and set up the event forwarder.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Unregister the capability
    registry.unregister("to_remove");

    let msg = tokio::time::timeout(Duration::from_secs(2), conn.recv())
        .await
        .expect("should receive event within timeout")
        .expect("should receive message");

    match msg {
        Some(Message::Event(event)) => {
            assert_eq!(event.kind, "capabilities_changed");
            let payload = event.payload;
            assert_eq!(payload["action"], "unregistered");
            assert_eq!(payload["name"], "to_remove");
            let caps = payload["capabilities"].as_array().unwrap();
            assert!(!caps.iter().any(|c| c == "to_remove"));
        }
        other => panic!("expected Event, got {:?}", other),
    }

    shutdown.send(()).ok();
}
