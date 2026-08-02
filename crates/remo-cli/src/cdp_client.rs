//! A thin, real-CDP client: dial a `ws://` URL (either directly over TCP, or
//! tunneled through usbmuxd for a wired device) and speak the same
//! `{"id","method","params"}` / `{"id","result"}` / `{"id","error"}` envelope
//! shape the embedded `remo-cdp` server (and real Chrome DevTools) use.
//!
//! This replaces the old `remo_desktop::RpcClient` + length-prefixed
//! `remo_protocol` framing entirely. There is no device-manager abstraction
//! here on purpose (see the rewrite plan's Phase 2 scope) — just enough to
//! resolve a target to a socket, dial it, and shuttle JSON frames.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::{TcpStream, UnixStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// The page id `remo-sdk`'s `RemoServer` always registers its one
/// inspectable target under (see `crates/remo-sdk/src/server.rs`) — Remo's
/// model is one app process, one target, so this never varies at runtime.
const DEVTOOLS_PAGE_ID: &str = "1";

/// How a [`CdpClient`] was dialed. Kept as an enum rather than a boxed trait
/// object: only two transports exist, and matching on them directly avoids
/// the extra indirection of separately boxing the `Sink` and `Stream` halves
/// of a `WebSocketStream<S>` for an arbitrary `S`.
enum Transport {
    Tcp(WebSocketStream<MaybeTlsStream<TcpStream>>),
    Usb(WebSocketStream<UnixStream>),
}

impl Transport {
    async fn send(&mut self, message: Message) -> Result<()> {
        match self {
            Transport::Tcp(ws) => ws.send(message).await,
            Transport::Usb(ws) => ws.send(message).await,
        }
        .context("failed to send CDP frame")
    }

    async fn recv(&mut self) -> Result<Option<Message>> {
        let next = match self {
            Transport::Tcp(ws) => ws.next().await,
            Transport::Usb(ws) => ws.next().await,
        };
        match next {
            Some(result) => Ok(Some(result.context("CDP connection error")?)),
            None => Ok(None),
        }
    }
}

/// A real CDP client speaking directly to the embedded `remo-cdp` server.
pub struct CdpClient {
    transport: Transport,
    next_id: u64,
}

impl CdpClient {
    /// Dials `addr` directly over TCP (simulator via Bonjour, or `--addr`).
    pub async fn connect_tcp(addr: SocketAddr) -> Result<Self> {
        let url = devtools_url(&addr.to_string());
        let (ws, _response) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("failed to open CDP WebSocket to {url}"))?;
        Ok(Self {
            transport: Transport::Tcp(ws),
            next_id: 1,
        })
    }

    /// Dials a real device by USB device id, tunneling the WebSocket
    /// handshake itself through usbmuxd (there is no direct TCP route to a
    /// real device — matching the security posture in the rewrite plan).
    pub async fn connect_usb(device_id: u32) -> Result<Self> {
        let usbmux = remo_usbmuxd::UsbmuxClient::connect()
            .await
            .context("failed to connect to usbmuxd")?;
        let port = remo_protocol::DEFAULT_PORT;
        let tunnel = usbmux
            .connect_to_device(device_id, port)
            .await
            .with_context(|| format!("failed to open usbmuxd tunnel to device {device_id}"))?;

        let url = devtools_url(&format!("127.0.0.1:{port}"));
        let (ws, _response) = tokio_tungstenite::client_async(&url, tunnel)
            .await
            .with_context(|| format!("CDP WebSocket handshake over USB tunnel failed ({url})"))?;
        Ok(Self {
            transport: Transport::Usb(ws),
            next_id: 1,
        })
    }

    /// Sends one CDP request and waits for the reply with the matching id.
    /// Any events (`{"method","params"}`, no `id`) seen while waiting are
    /// silently skipped — this client doesn't watch for unsolicited events.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let frame = json!({ "id": id, "method": method, "params": params });
        self.transport
            .send(Message::Text(frame.to_string().into()))
            .await?;

        loop {
            let message = self
                .transport
                .recv()
                .await?
                .ok_or_else(|| anyhow!("CDP connection closed before a reply arrived"))?;
            let Message::Text(text) = message else {
                continue;
            };
            let envelope: Value = serde_json::from_str(&text)
                .with_context(|| format!("unparseable CDP frame: {text}"))?;
            if envelope.get("id").and_then(Value::as_u64) != Some(id) {
                // Not our reply (an event, or a reply to a stale in-flight
                // call this client never made) — keep waiting.
                continue;
            }
            if let Some(error) = envelope.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown CDP error");
                bail!("{message}");
            }
            return Ok(envelope.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Calls the custom `Remo.invoke` method — the actual product: invoking
    /// a developer-registered, named capability inside the app. Unwraps the
    /// extra `{"result": <capability's value>}` layer `Remo.invoke` adds on
    /// top of the outer CDP `{"id","result"}` envelope (see
    /// `remo-cdp/src/domain_remo.rs`).
    pub async fn invoke_capability(&mut self, name: &str, args: Value) -> Result<Value> {
        let reply = self
            .call("Remo.invoke", json!({ "name": name, "args": args }))
            .await
            .with_context(|| format!("capability '{name}' failed"))?;
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Calls `Remo.listCapabilities`, returning every currently-invokable
    /// capability name.
    pub async fn list_capabilities(&mut self) -> Result<Vec<String>> {
        let reply = self.call("Remo.listCapabilities", json!({})).await?;
        let names = reply
            .get("names")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("Remo.listCapabilities reply missing \"names\" array"))?
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect();
        Ok(names)
    }

    /// Calls the standard `Page.captureScreenshot` domain method directly
    /// (rather than the legacy `Remo.invoke`-wrapped `__screenshot`
    /// capability) — this is real CDP surface any client already knows how
    /// to call, and `remo-cdp`'s `PageDomain` backs it with the same
    /// `remo_objc::capture_screenshot` path the old capability used.
    pub async fn capture_screenshot(&mut self, format: &str, quality: f64) -> Result<Vec<u8>> {
        let reply = self
            .call(
                "Page.captureScreenshot",
                json!({ "format": format, "quality": (quality * 100.0).round() as u64 }),
            )
            .await?;
        let data = reply
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Page.captureScreenshot reply missing \"data\""))?;
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
            .context("screenshot data was not valid base64")
    }
}

/// Waits up to `timeout` for a single call, surfacing a clear error instead
/// of hanging forever if the app-side capability never answers.
pub async fn with_timeout<T>(
    timeout: Duration,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(timeout, future)
        .await
        .context("timed out waiting for a reply")?
}

fn devtools_url(host_port: &str) -> String {
    format!("ws://{host_port}/devtools/page/{DEVTOOLS_PAGE_ID}")
}
