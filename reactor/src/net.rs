use std::{
    io::{self, Read, Write},
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{channel::mpsc::UnboundedReceiver, ready, AsyncRead, AsyncWrite, Future, StreamExt};
use mio::Interest;

use crate::io::{Event, Registration};

/// Listener for TCP events
pub struct TcpListener {
    registration: Registration<mio::net::TcpListener>,
}

impl TcpListener {
    /// Create a new TcpListener bound to the socket
    pub fn bind(addr: SocketAddr) -> std::io::Result<Self> {
        let listener = mio::net::TcpListener::bind(addr)?;
        let registration = Registration::new(listener, Interest::READABLE)?;
        Ok(Self { registration })
    }

    /// Accept a new TcpStream to communicate with
    pub fn accept(&self) -> Accept<'_> {
        let events = self.registration.events();
        let source = &*self.registration;
        Accept { events, source }
    }
}

pub struct Accept<'a> {
    events: UnboundedReceiver<Event>,
    source: &'a mio::net::TcpListener,
}

impl Unpin for Accept<'_> {}

impl Future for Accept<'_> {
    type Output = io::Result<(TcpStream, SocketAddr)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        ready!(self.events.poll_next_unpin(cx));
        match self.source.accept() {
            Ok((stream, socket)) => Poll::Ready(Ok((TcpStream::from_mio(stream)?, socket))),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Poll::Pending,
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

/// Handles communication over a TCP connection
pub struct TcpStream {
    registration: Registration<mio::net::TcpStream>,

    readable: Option<()>,
    writable: Option<()>,

    events: UnboundedReceiver<Event>,
}

impl Unpin for TcpStream {}

impl TcpStream {
    pub(crate) fn from_mio(stream: mio::net::TcpStream) -> std::io::Result<Self> {
        // register the stream to the OS
        let registration = Registration::new(stream, Interest::READABLE | Interest::WRITABLE)?;
        let events = registration.events();
        Ok(Self {
            registration,
            readable: Some(()),
            writable: Some(()),
            events,
        })
    }

    fn poll_event(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let event = match self.events.poll_next_unpin(cx) {
            Poll::Ready(Some(event)) => event,
            Poll::Ready(None) => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "channel disconnected",
                )))
            }
            Poll::Pending => return Poll::Pending,
        };

        if event.is_readable() {
            self.readable = Some(());
        }
        if event.is_writable() {
            self.writable = Some(());
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            // if the stream is readable
            if let Some(()) = self.readable.take() {
                // try read some bytes
                match self.registration.read(buf) {
                    Ok(n) => {
                        // ensure that we attempt another read next time
                        // since no new readable events will come through
                        // https://docs.rs/mio/0.8.0/mio/struct.Poll.html#draining-readiness
                        self.readable = Some(());
                        return Poll::Ready(Ok(n));
                    }
                    // if reading would block the thread, continue to event polling
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    // if there was some other io error, bail
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }

            match self.as_mut().poll_event(cx)? {
                Poll::Ready(()) => {}
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            // if the stream is writeable
            if let Some(()) = self.writable.take() {
                // try write some bytes
                match self.registration.write(buf) {
                    Ok(n) => {
                        // ensure that we attempt another write next time
                        // since no new writeable events will come through
                        // https://docs.rs/mio/0.8.0/mio/struct.Poll.html#draining-readiness
                        self.writable = Some(());
                        return Poll::Ready(Ok(n));
                    }
                    // if writing would block the thread, continue to event polling
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    // if there was some other io error, bail
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }

            match self.as_mut().poll_event(cx)? {
                Poll::Ready(()) => {}
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        loop {
            // if the stream is writeable
            if let Some(()) = self.writable.take() {
                // try flush the bytes
                match self.registration.flush() {
                    Ok(()) => {
                        // ensure that we attempt another write next time
                        // since no new writeable events will come through
                        // https://docs.rs/mio/0.8.0/mio/struct.Poll.html#draining-readiness
                        self.writable = Some(());
                        return Poll::Ready(Ok(()));
                    }
                    // if flushing would block the thread, continue to event polling
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    // if there was some other io error, bail
                    Err(e) => return Poll::Ready(Err(e)),
                }
            }

            match self.as_mut().poll_event(cx)? {
                Poll::Ready(()) => {}
                Poll::Pending => return Poll::Pending,
            }
        }
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // shutdowns are immediate
        Poll::Ready(self.registration.shutdown(std::net::Shutdown::Write))
    }
}
