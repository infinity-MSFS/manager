//! A listener whose connections end when a token is cancelled.
//!
//! axum's graceful shutdown stops accepting and drains plain HTTP requests,
//! but an upgraded WebSocket has left hyper by then: its task is detached
//! and keeps the gauge's socket open against a server that no longer exists.
//! The gauge never sees a close, so it never reconnects to the next server on
//! that port. Cutting the IO underneath ends every socket the listener ever
//! produced, upgraded or not.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

#[derive(Debug)]
pub(crate) struct CancellableListener {
    inner: TcpListener,
    token: CancellationToken,
}

impl CancellableListener {
    pub(crate) fn new(inner: TcpListener, token: CancellationToken) -> Self {
        Self { inner, token }
    }
}

impl axum::serve::Listener for CancellableListener {
    type Io = CancellableIo;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (io, addr) = axum::serve::Listener::accept(&mut self.inner).await;
        (CancellableIo::new(io, self.token.clone()), addr)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// Reads EOF and refuses writes once the token is cancelled.
#[derive(Debug)]
pub(crate) struct CancellableIo {
    inner: TcpStream,
    token: CancellationToken,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl CancellableIo {
    fn new(inner: TcpStream, token: CancellationToken) -> Self {
        let cancelled = Box::pin(token.clone().cancelled_owned());
        Self {
            inner,
            token,
            cancelled,
        }
    }

    /// Registers the waker, so a reader parked on a silent socket still
    /// wakes when shutdown starts.
    fn poll_cancelled(&mut self, cx: &mut Context<'_>) -> bool {
        // Checked first: the future must not be polled again once complete.
        self.token.is_cancelled() || self.cancelled.as_mut().poll(cx).is_ready()
    }
}

impl AsyncRead for CancellableIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.poll_cancelled(cx) {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for CancellableIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.token.is_cancelled() {
            return Poll::Ready(Err(io::ErrorKind::ConnectionAborted.into()));
        }
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
