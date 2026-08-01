use std::net::SocketAddr;

use tokio::net::{TcpListener, TcpStream};
use tracing::info;

use crate::connection::Connection;
use crate::TransportError;

/// A TCP server that accepts incoming `Connection`s.
pub struct Listener {
    inner: TcpListener,
    local_addr: SocketAddr,
}

impl Listener {
    /// Bind to the given address.
    pub async fn bind(addr: impl Into<SocketAddr>) -> Result<Self, TransportError> {
        let addr = addr.into();
        let inner = TcpListener::bind(addr).await?;
        let local_addr = inner.local_addr()?;
        info!(%local_addr, "remo listener started");
        Ok(Self { inner, local_addr })
    }

    /// Accept the next connection, already wrapped in the legacy
    /// length-prefixed codec. Existing callers keep this exact behavior.
    pub async fn accept(&self) -> Result<Connection, TransportError> {
        let (stream, peer) = self.accept_raw().await?;
        info!(%peer, "accepted connection");
        Connection::new(stream)
    }

    /// Accept the next connection as a raw, unwrapped stream — the seam a
    /// dual-stack caller needs: peek the first bytes *before* deciding
    /// whether to wrap it in the legacy codec (`Connection::new`) or hand it
    /// to a different protocol entirely. Once a stream is wrapped in
    /// `Connection`, that decision can no longer be made — the codec has
    /// already taken ownership of it.
    pub async fn accept_raw(&self) -> Result<(TcpStream, SocketAddr), TransportError> {
        let (stream, peer) = self.inner.accept().await?;
        Ok((stream, peer))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}
