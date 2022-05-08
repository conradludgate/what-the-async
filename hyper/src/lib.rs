use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{ready, AsyncRead, AsyncWrite, Future, FutureExt};
use wta_reactor::net::{Accept, TcpListener, TcpStream};

pub struct Incoming<'a> {
    listener: &'a TcpListener,
    accept: Option<Accept<'a>>,
}

impl<'a> Incoming<'a> {
    pub fn new(listener: &'a TcpListener) -> Self {
        Self {
            listener,
            accept: None,
        }
    }
}

impl hyper::server::accept::Accept for Incoming<'_> {
    type Conn = AddrStream;

    type Error = std::io::Error;

    fn poll_accept(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        let mut accept = self.accept.take().unwrap_or_else(|| self.listener.accept());
        match accept.poll_unpin(cx) {
            Poll::Ready(Ok((stream, socket))) => {
                Poll::Ready(Some(Ok(AddrStream { stream, socket })))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Pending => {
                self.accept.replace(accept);
                Poll::Pending
            }
        }
    }
}

/// what-the-async executor
#[derive(Clone)]
pub struct Executor;
impl<F> hyper::rt::Executor<F> for Executor
where
    F: Future + Send + Sync + 'static,
    F::Output: Send,
{
    fn execute(&self, fut: F) {
        what_the_async::spawn(fut);
    }
}

pub struct AddrStream {
    stream: TcpStream,
    socket: SocketAddr,
}

impl AddrStream {
    pub fn remote_addr(&self) -> SocketAddr {
        self.socket
    }
}

impl tokio::io::AsyncRead for AddrStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let pin = Pin::new(&mut self.stream);
        let n = ready!(pin.poll_read(cx, buf.initialize_unfilled())?);
        buf.advance(n);
        Poll::Ready(Ok(()))
    }
}

impl tokio::io::AsyncWrite for AddrStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let pin = Pin::new(&mut self.stream);
        pin.poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let pin = Pin::new(&mut self.stream);
        pin.poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let pin = Pin::new(&mut self.stream);
        pin.poll_close(cx)
    }
}
