//! Proves `looks_like_http`/`serve_on_stream` against real sockets — this is
//! exactly the logic a dual-stack `remo-sdk` accept loop will lean on to
//! decide, per connection, whether to hand it to this crate or to the
//! legacy length-prefixed codec, so it needs to be right against real
//! bytes, not just plausible on paper.

use remo_cdp::dual_stack::{looks_like_http, serve_on_stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn bind_loopback() -> TcpListener {
    TcpListener::bind(("127.0.0.1", 0)).await.unwrap()
}

#[tokio::test]
async fn detects_a_real_http_request_line() {
    let listener = bind_loopback().await;
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        looks_like_http(&stream).await.unwrap()
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /json/version HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();

    assert!(
        server.await.unwrap(),
        "a real GET request must be detected as HTTP"
    );
}

#[tokio::test]
async fn does_not_misdetect_the_legacy_length_prefixed_frame() {
    let listener = bind_loopback().await;
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        looks_like_http(&stream).await.unwrap()
    });

    // A representative legacy frame: 4-byte BE length, then a JSON type byte
    // (0x00), then arbitrary payload bytes — none of which spell an HTTP verb.
    let mut client = TcpStream::connect(addr).await.unwrap();
    let mut frame = vec![0u8, 0, 0, 42, 0x00];
    frame.extend_from_slice(br#"{"type":"request"}"#);
    client.write_all(&frame).await.unwrap();

    assert!(
        !server.await.unwrap(),
        "a legacy length-prefixed frame must never be mistaken for HTTP"
    );
}

#[tokio::test]
async fn peek_does_not_consume_bytes_the_legacy_codec_still_needs() {
    // Whichever branch a real accept loop takes, the bytes peeked here must
    // still be readable afterward — `TcpStream::peek` is non-destructive by
    // contract, but this proves it end to end rather than trusting the docs.
    let listener = bind_loopback().await;
    let addr = listener.local_addr().unwrap();
    let payload = b"not http at all, just bytes";

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let is_http = looks_like_http(&stream).await.unwrap();
        let mut buf = vec![0u8; payload.len()];
        stream.read_exact(&mut buf).await.unwrap();
        (is_http, buf)
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    client.write_all(payload).await.unwrap();

    let (is_http, read_back) = server.await.unwrap();
    assert!(!is_http);
    assert_eq!(&read_back, payload);
}

#[tokio::test]
async fn a_silent_connection_is_treated_as_legacy_not_hung_forever() {
    // Reproduces a real bug found by running the full workspace test suite,
    // not by inspection: a legacy client that connects and only *listens*
    // for server-pushed events (never sends a request) used to hang the
    // peek forever, since it waited for bytes that were never coming. This
    // is exactly that shape, minus the actual event machinery — connect,
    // send nothing, and the peek must still resolve (to `false`) well before
    // a human would notice, not eventually.
    let listener = bind_loopback().await;
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        looks_like_http(&stream).await.unwrap()
    });

    let client = TcpStream::connect(addr).await.unwrap();
    // Deliberately never write anything — hold the connection open silently,
    // the way an event-only legacy client does.

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .expect("looks_like_http must resolve on its own timeout, not hang")
        .unwrap();
    assert!(
        !result,
        "a silent connection must be treated as legacy, not CDP"
    );
    drop(client);
}

#[tokio::test]
async fn serves_discovery_over_a_manually_accepted_stream() {
    let listener = bind_loopback().await;
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let router = remo_cdp::discovery::router(remo_cdp::discovery::DiscoveryConfig {
            page_title: "test".to_string(),
            page_id: "1".to_string(),
        });
        serve_on_stream(stream, router).await;
    });

    // A tiny raw HTTP/1.0 request avoids pulling in a full HTTP client crate
    // just for this one assertion.
    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /json/version HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();

    assert!(response.contains("200 OK"), "response: {response}");
    assert!(
        response.contains("remoProtocolVersion"),
        "response: {response}"
    );
}
