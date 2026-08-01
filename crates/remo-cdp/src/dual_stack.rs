//! Serves an axum [`Router`] over a socket that was already `accept()`ed by
//! someone else's listener, and the byte-peek that decides whether a given
//! connection is this new HTTP/WebSocket protocol at all.
//!
//! Exists so `remo-sdk`'s existing accept loop can keep owning the
//! `TcpListener` (old clients must keep working during the migration) while
//! routing individual connections to either the legacy length-prefixed codec
//! or this crate's CDP server — see the rewrite plan's Phase 1: "dual-stack,
//! not a flag day".

use std::time::Duration;

use axum::Router;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

/// Old framing starts with a 4-byte big-endian length prefix; new framing
/// starts with an HTTP request line (`GET /json/version HTTP/1.1`, or a
/// WebSocket upgrade `GET /devtools/page/1 HTTP/1.1`). Real CDP clients only
/// ever open a plain HTTP connection first — there is no other verb in
/// practice — so checking for `GET ` (and `HEAD `/`POST ` defensively, in
/// case a future discovery step needs one) is sufficient and cheap.
///
/// A length prefix colliding with these four bytes would require an
/// astronomically large frame (the ASCII bytes `"GET "` read as a
/// big-endian `u32` is over 1.1 billion) — far past the 64 MiB frame limit
/// the legacy codec already enforces, so even a theoretical collision would
/// be rejected by the old path immediately rather than silently misroute.
const HTTP_METHOD_PREFIXES: &[&[u8]] = &[b"GET ", b"HEAD ", b"POST "];

/// A real client of *either* protocol sends its first bytes essentially
/// immediately after connecting — an HTTP request line, or (today) the
/// first framed `Request`. This is generous relative to that, not a
/// meaningful wait.
const PEEK_TIMEOUT: Duration = Duration::from_millis(500);

/// Peeks (does not consume) enough bytes to tell the two protocols apart.
/// Returns `true` if this connection should be handed to
/// [`serve_on_stream`] instead of the legacy codec.
///
/// One legacy usage this has to account for: a client may connect and then
/// send nothing at all for a while, only listening for server-pushed events
/// (`capabilities_changed` and friends) — silence is not evidence of
/// anything. Without a timeout, peeking would hang forever waiting for bytes
/// that were never coming, wedging that connection's accept-loop task
/// permanently. A real CDP client, by contrast, always speaks first. So:
/// bytes arrive and match → CDP; bytes arrive and don't match → legacy;
/// nothing arrives within the timeout → legacy (a silent, event-only legacy
/// client is a real, supported shape; a silent CDP client is not — Chrome's
/// frontend and any raw CDP client always send a request immediately).
pub async fn looks_like_http(stream: &TcpStream) -> std::io::Result<bool> {
    let mut buf = [0u8; 8];
    match tokio::time::timeout(PEEK_TIMEOUT, peek_full(stream, &mut buf)).await {
        Ok(Ok(peeked)) => Ok(HTTP_METHOD_PREFIXES
            .iter()
            .any(|prefix| peeked.starts_with(prefix))),
        Ok(Err(io_error)) => Err(io_error),
        Err(_elapsed) => Ok(false),
    }
}

/// `TcpStream::peek` can return fewer bytes than requested on a single call
/// (it's still just a socket read under the hood) — loop until the buffer
/// fills or the peer closes without sending enough to decide, in which case
/// whatever arrived is enough context; a real client always sends its
/// method line in one write.
async fn peek_full<'a>(stream: &TcpStream, buf: &'a mut [u8]) -> std::io::Result<&'a [u8]> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = stream.peek(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(&buf[..filled])
}

/// Serves one HTTP/1.1 connection — including a WebSocket upgrade partway
/// through it — from an already-accepted [`TcpStream`]. `axum::serve` can't
/// be used directly here: it wants to own the `TcpListener` itself and
/// accept its own connections, but this stream was already accepted (and
/// peeked) by the caller's own loop.
pub async fn serve_on_stream(stream: TcpStream, router: Router) {
    let io = TokioIo::new(stream);
    let service = hyper_util::service::TowerToHyperService::new(router);
    // `with_upgrades()` is not optional: without it, hyper closes the
    // connection the moment the initial HTTP response is sent, which is
    // exactly when a WebSocket upgrade needs to keep it open instead.
    if let Err(error) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, service)
        .with_upgrades()
        .await
    {
        tracing::warn!(%error, "cdp http/ws connection error");
    }
}
