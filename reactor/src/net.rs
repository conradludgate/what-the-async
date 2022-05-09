use std::{
    io::{self, Read, Write},
    mem::replace,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{ready, AsyncRead, AsyncWrite, Stream, StreamExt};
use mio::Interest;

use crate::io::Registration;

/// Listener for TCP connectors
pub struct TcpListener {
    registration: Registration<mio::net::TcpListener>,
}

impl TcpListener {
    /// Create a new `TcpListener` bound to the socket
    pub fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let registration =
            Registration::new(mio::net::TcpListener::bind(addr)?, Interest::READABLE)?;
        Ok(Self { registration })
    }

    /// Accept [`TcpStream`]s to communicate with
    pub fn accept(self) -> Accept {
        let Self { registration } = self;
        Accept { registration }
    }
}

/// A [`Stream`] of [`TcpStream`] that are connecting to this tcp server
pub struct Accept {
    registration: Registration<mio::net::TcpListener>,
}
impl Unpin for Accept {}

impl Stream for Accept {
    type Item = io::Result<(TcpStream, SocketAddr)>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if ready!(self.registration.events.poll_next_unpin(cx)).is_none() {
            return Poll::Ready(None);
        }
        match self.registration.accept() {
            Ok((stream, socket)) => Poll::Ready(Some(Ok((TcpStream::from_mio(stream)?, socket)))),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Poll::Pending,
            Err(e) => Poll::Ready(Some(Err(e))),
        }
    }
}

/// Handles communication over a TCP connection
pub struct TcpStream {
    registration: Registration<mio::net::TcpStream>,
    read: bool,
    write: bool,
}

impl Unpin for TcpStream {}

impl TcpStream {
    pub(crate) fn from_mio(stream: mio::net::TcpStream) -> std::io::Result<Self> {
        // register the stream to the OS
        let registration = Registration::new(stream, Interest::READABLE | Interest::WRITABLE)?;
        Ok(Self {
            registration,
            read: true,
            write: true,
        })
    }

    fn poll_event(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let event = match ready!(self.registration.events.poll_next_unpin(cx)) {
            Some(event) => event,
            None => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "channel disconnected",
                )))
            }
        };
        self.read |= event.is_readable();
        self.write |= event.is_writable();
        Poll::Ready(Ok(()))
    }
}

// polls the OS events until either there are no more events, or the IO does not block
macro_rules! poll_io {
    ($self:ident, $cx:ident @ $mode:ident: let $pat:pat = $io:expr; $expr:expr) => {
        loop {
            if replace(&mut $self.$mode, false) {
                match $io {
                    Ok($pat) => {
                        // ensure that we attempt another read/write next time
                        // since no new events will come through
                        // https://docs.rs/mio/0.8.0/mio/struct.Poll.html#draining-readiness
                        $self.$mode = true;
                        return Poll::Ready($expr);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }
            ready!($self.as_mut().poll_event($cx)?)
        }
    };
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        poll_io!(self, cx @ read: let n = self.registration.read(buf); Ok(n))
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        poll_io!(self, cx @ write: let n = self.registration.write(buf); Ok(n))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        poll_io!(self, cx @ write: let _ = self.registration.flush(); Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(self.registration.shutdown(std::net::Shutdown::Write))
    }
}
