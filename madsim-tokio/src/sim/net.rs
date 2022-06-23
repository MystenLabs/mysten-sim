
use std::{
    future::Future,
    io,
    net::{SocketAddr, ToSocketAddrs},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

type PollerPinFut<R> = Pin<Box<dyn Future<Output = R>>>;
struct Poller<R> {
    p: Arc<Mutex<Option<PollerPinFut<R>>>>,
}

impl<R> Poller<R> {
    fn new() -> Self {
        Self {
            p: Arc::new(Mutex::new(None)),
        }
    }

    fn poll_with_fut<F, T>(&self, cx: &mut Context<'_>, fut_fn: F) -> Poll<R>
        where
            F: FnOnce() -> T,
            T: Future<Output = R> + 'static
    {
        let mut poller = self.p.lock().unwrap();

        if poller.is_none() {
            *poller = Some(Box::pin(fut_fn()));
        }

        let fut: &mut PollerPinFut<R> = poller.as_mut().unwrap();
        let res = fut.as_mut().poll(cx);

        if let Poll::Ready(_) = res {
            *poller = None;
        }
        res
    }
}

/// Provide the tokio::net::TcpListener interface.
pub struct TcpListener {
    ep: Endpoint,
    poller: Poller<io::Result<(TcpStream, SocketAddr)>>,
}

impl std::fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        todo!()
    }
}

/// Provide the tokio::net::TcpListener interface.
impl TcpListener {

    /// Bind to the given address.
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let addrs = addr.to_socket_addrs()?;

        let mut last_err = None;

        for addr in addrs {
            match Self::bind_addr(addr).await {
                Ok(listener) => return Ok(listener),
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        }))
    }

    async fn bind_addr(addr: SocketAddr) -> io::Result<Self> {
        let ep = Endpoint::bind(addr).await?;
        Ok(Self {
            ep,
            poller: Poller::new(),
        })
    }

    /// poll_accept
    pub fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<io::Result<(TcpStream, SocketAddr)>> {
        self.poller.poll_with_fut(cx, || {
            Self::poll_accept_internal(self.ep.clone())
        })
    }

    async fn poll_accept_internal(ep: Endpoint) -> io::Result<(TcpStream, SocketAddr)> {
        let (msg, from) = ep.recv_from_raw(0).await?;
        let stream = TcpStream::new_with_initial_msg(ep, msg);
        Ok((stream, from))
    }
}

/// Provide the tokio::net::TcpStream interface.
#[derive(Debug)]
pub struct TcpStream {

}

impl TcpStream {
    fn new_with_initial_msg(_msg: Payload) -> Self {
        todo!()
    }

    async fn read(&self) -> io::Result<(Payload, SocketAddr)> {
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.poll_read_priv(cx, buf)
    }
}


/// Provide the tokio::net::UnixListener interface.
#[derive(Debug)]
pub struct UnixListener {
}

impl UnixListener {
    /// poll_accept
    pub fn poll_accept(&self, _cx: &mut Context<'_>) -> Poll<io::Result<(UnixStream, SocketAddr)>> {
        todo!()
    }
}

/// Provide the tokio::net::UnixStream interface.
#[derive(Debug)]
pub struct UnixStream {
}

