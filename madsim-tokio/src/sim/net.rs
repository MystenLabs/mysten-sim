use log::trace;

use std::{
    future::Future,
    io,
    net::{SocketAddr, ToSocketAddrs},
    pin::Pin,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
};

use madsim::net::{network::Payload, Endpoint};

use real_tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub use super::unix::*;

type PollerPinFut<R> = Pin<Box<dyn Future<Output = R> + Send + Sync>>;
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
        T: Future<Output = R> + 'static + Send + Sync,
    {
        let mut poller = self.p.lock().unwrap();

        if poller.is_none() {
            *poller = Some(Box::pin(fut_fn()));
        }

        let fut: &mut PollerPinFut<R> = poller.as_mut().unwrap();
        let res = fut.as_mut().poll(cx);

        if let Poll::Ready(_) = res {
            poller.take();
        }
        res
    }
}

/// Provide the tokio::net::TcpListener interface.
pub struct TcpListener {
    ep: Arc<Endpoint>,
    poller: Poller<io::Result<(TcpStream, SocketAddr)>>,
}

impl std::fmt::Debug for TcpListener {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        fmt.debug_struct("TcpListener")
            .field("ep", &self.ep)
            .finish()
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
        let ep = Arc::new(Endpoint::bind(addr).await?);
        Ok(Self {
            ep,
            poller: Poller::new(),
        })
    }

    /// poll_accept
    pub fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<io::Result<(TcpStream, SocketAddr)>> {
        self.poller
            .poll_with_fut(cx, || Self::poll_accept_internal(self.ep.clone()))
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        Self::poll_accept_internal(self.ep.clone()).await
    }

    async fn poll_accept_internal(ep: Arc<Endpoint>) -> io::Result<(TcpStream, SocketAddr)> {
        let (msg, from) = ep.recv_from_raw(0).await?;

        let remote_port = Message::new(msg).unwrap_port();
        trace!(
            "server: read remote port from client {} {}",
            from,
            remote_port
        );

        let local_port: u32 = ep.allocate_local_port();

        let state = TcpState::new(ep, local_port, remote_port, from);

        // tell other side what our port is
        trace!(
            "server: sending local_port {} to client {}-{}",
            local_port,
            from,
            remote_port
        );
        state
            .ep
            .send_to_raw(from, state.next_send_tag(), Message::port(local_port))
            .await?;

        let stream = TcpStream::new(state);
        Ok((stream, from))
    }
}

struct Buffer {
    buffer: Vec<u8>,
    buffer_cursor: usize,
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        write!(fmt, "Buffer {:?}", self.remaining())
    }
}

impl Buffer {
    fn new_from_vec(buffer: Vec<u8>) -> Self {
        Self {
            buffer,
            buffer_cursor: 0,
        }
    }

    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            buffer_cursor: 0,
        }
    }

    fn remaining_bytes(&self) -> usize {
        self.buffer.len() - self.buffer_cursor
    }

    fn remaining(&self) -> &[u8] {
        &self.buffer[self.buffer_cursor..self.buffer.len()]
    }

    fn read(&mut self, read: &mut ReadBuf<'_>) -> usize {
        let remaining_bytes = self.remaining_bytes();
        let to_write = std::cmp::min(remaining_bytes, read.remaining());
        if to_write > 0 {
            read.put_slice(&self.buffer[self.buffer_cursor..(self.buffer_cursor + to_write)]);
        }
        self.buffer_cursor += to_write;

        if self.remaining_bytes() == 0 {
            self.buffer_cursor = 0;
            self.buffer.clear();
        }

        to_write
    }

    fn write(&mut self, buf: Buffer) {
        self.buffer.extend_from_slice(buf.remaining());
    }
}

enum Message {
    Port(u32),
    // sequence number + payload - the simulator is unordered.
    // sequence number is unnecessary, but we use it for a debug assert to make sure we haven't
    // screwed up the sequencing.
    Payload(u32, Vec<u8>),
}

impl Message {
    fn new(payload: Payload) -> Self {
        *payload.downcast::<Message>().unwrap()
    }

    fn port(p: u32) -> Box<Message> {
        Box::new(Message::Port(p))
    }

    fn payload(s: u32, v: Vec<u8>) -> Box<Message> {
        Box::new(Message::Payload(s, v))
    }

    fn unwrap_payload(self) -> (u32, Vec<u8>) {
        match self {
            Message::Payload(s, v) => (s, v),
            _ => panic!("expected payload"),
        }
    }

    fn unwrap_port(self) -> u32 {
        match self {
            Message::Port(p) => p,
            _ => panic!("expected port"),
        }
    }
}

#[derive(Debug)]
struct TcpState {
    ep: Arc<Endpoint>,
    send_seq: AtomicU32,
    recv_seq: AtomicU32,
    local_port: u32,
    remote_port: u32,
    remote_sock: SocketAddr,
}

impl TcpState {
    fn new(ep: Arc<Endpoint>, local_port: u32, remote_port: u32, remote_sock: SocketAddr) -> Self {
        Self {
            ep,
            send_seq: AtomicU32::new(0),
            recv_seq: AtomicU32::new(0),
            local_port,
            remote_port,
            remote_sock,
        }
    }

    fn next_send_tag(&self) -> u64 {
        let seq = self.send_seq.fetch_add(1, Ordering::SeqCst) as u64;
        let port = self.remote_port as u64;
        (port << 32) | seq
    }

    fn next_recv_tag(&self) -> u64 {
        let seq = self.recv_seq.fetch_add(1, Ordering::SeqCst) as u64;
        let port = self.local_port as u64;
        (port << 32) | seq
    }
}

/// Provide the tokio::net::TcpStream interface.
pub struct TcpStream {
    state: Arc<TcpState>,
    read_poller: Poller<io::Result<Buffer>>,
    write_poller: Poller<io::Result<usize>>,
    buffer: Mutex<Buffer>,
}

impl std::fmt::Debug for TcpStream {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        fmt.debug_struct("TcpStream")
            .field("state", &self.state)
            .field("buffer", &self.buffer)
            .finish()
    }
}

impl TcpStream {
    fn new(state: TcpState) -> Self {
        Self {
            state: Arc::new(state),
            read_poller: Poller::new(),
            write_poller: Poller::new(),
            buffer: Mutex::new(Buffer::new()),
        }
    }

    pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        match TcpStream::connect_addr(addr).await {
            Ok(stream) => return Ok(stream),
            Err(e) => Err(e),
        }
    }

    async fn connect_addr(addr: impl ToSocketAddrs) -> io::Result<TcpStream> {
        let ep = Arc::new(Endpoint::connect(addr).await?);
        trace!("connect {:?}", ep.local_addr());

        let remote_sock = ep.peer_addr()?;
        let local_port = ep.allocate_local_port();

        // partially initialized state
        let mut state = TcpState::new(ep, local_port, 0xdeadbeef, remote_sock);

        // establish new connection, use reserved tag 0 to tell other side the local port.
        trace!("sending server {} localport {}", remote_sock, local_port);
        state
            .ep
            .send_to_raw(remote_sock, 0, Message::port(state.local_port))
            .await?;

        // read the remote port back
        trace!(
            "reading remote port from server {} {}",
            remote_sock,
            local_port
        );

        let (msg, from) = state.ep.recv_from_raw(state.next_recv_tag()).await?;
        debug_assert_eq!(from, remote_sock);
        // finish initializing state
        state.remote_port = Message::new(msg).unwrap_port();
        Ok(Self::new(state))
    }

    async fn read(state: Arc<TcpState>) -> io::Result<Buffer> {
        let tag = state.next_recv_tag();

        let (payload, from) = state.ep.recv_from_raw(tag).await?;
        debug_assert_eq!(from, state.remote_sock);

        let (seq, payload) = Message::new(payload).unwrap_payload();
        debug_assert_eq!(seq as u64, tag & 0xffffffff);
        Ok(Buffer::new_from_vec(payload))
    }

    async fn write(state: Arc<TcpState>, buf: Vec<u8>) -> io::Result<usize> {
        let num = buf.len();
        let tag = state.next_send_tag();
        let seq = (tag & 0xffffffff) as u32;
        state
            .ep
            .send_to_raw(state.remote_sock, tag, Message::payload(seq, buf))
            .await?;
        Ok(num)
    }

    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        split_owned(self)
    }

    fn poll_write_priv(&self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        self.write_poller
            .poll_with_fut(cx, || Self::write(self.state.clone(), buf.to_vec()))
    }

    fn poll_read_priv(&self, cx: &mut Context<'_>, read: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        debug_assert_ne!(read.remaining(), 0);

        let mut buffer = self.buffer.lock().unwrap();

        if buffer.read(read) > 0 {
            // We might be able to read more from the network right now,
            // but for simplicity we just return immediately if there was anything in the buffer.
            return Poll::Ready(Ok(()));
        }

        let poll = self
            .read_poller
            .poll_with_fut(cx, || Self::read(self.state.clone()));

        match poll {
            Poll::Ready(Ok(mut buf)) => {
                // fill buffer with whatever we can, and save remainder for later.
                buf.read(read);
                buffer.write(buf);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    // flush and shutdown are no-ops
    fn poll_flush_priv(&self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown_priv(&self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        // TODO: implement?
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        read: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.poll_read_priv(cx, read)
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_priv(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush_priv(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_shutdown_priv(cx)
    }
}

pub struct OwnedWriteHalf {
    inner: Arc<TcpStream>,
    // TODO: support this
    _shutdown_on_drop: bool,
}

impl AsyncWrite for OwnedWriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_write_priv(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.inner.poll_flush_priv(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.inner.poll_shutdown_priv(cx)
    }
}

pub struct OwnedReadHalf {
    inner: Arc<TcpStream>,
}

impl AsyncRead for OwnedReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        read: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.inner.poll_read_priv(cx, read)
    }
}

fn split_owned(stream: TcpStream) -> (OwnedReadHalf, OwnedWriteHalf) {
    let arc = Arc::new(stream);
    let read = OwnedReadHalf {
        inner: Arc::clone(&arc),
    };
    let write = OwnedWriteHalf {
        inner: arc,
        _shutdown_on_drop: true,
    };
    (read, write)
}

#[cfg(test)]
mod tests {

    use super::{OwnedReadHalf, OwnedWriteHalf, TcpListener, TcpStream};
    use bytes::{BufMut, BytesMut};
    use futures::join;
    use log::trace;
    use madsim::{rand, rand::RngCore, runtime::Runtime};
    use real_tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::Barrier,
    };
    use std::{net::SocketAddr, sync::Arc};

    async fn test_stream_read(mut stream: OwnedReadHalf) {
        trace!("test_stream_read");
        let mut rand = rand::thread_rng();

        let mut next_read: u8 = 0;

        loop {
            let read_size = (rand.next_u32() % 0xff).try_into().unwrap();
            let mut read_buf = BytesMut::with_capacity(read_size);
            stream.read_buf(&mut read_buf).await.unwrap();

            for byte in &read_buf {
                if *byte == 0xff {
                    return;
                }
                assert_eq!(*byte, next_read);
                next_read += 1;
                next_read %= 0xfe;
            }
        }
    }

    async fn test_stream_write(mut stream: OwnedWriteHalf) {
        trace!("test_stream_write");
        let mut rand = rand::thread_rng();

        let mut next_write: u8 = 0;

        for _ in 0..2048 {
            let write_size: usize = (rand.next_u32() % 0xff).try_into().unwrap();
            let mut write_buf = BytesMut::with_capacity(write_size + 1);
            for _ in 0..write_size {
                write_buf.put_u8(next_write);
                next_write += 1;
                next_write %= 0xfe
            }
            stream.write_all_buf(&mut write_buf).await.unwrap();
        }
        stream.write_u8(0xff).await.unwrap();
    }

    #[test]
    fn tcp_ping_pong() {
        let runtime = Runtime::new();
        let addr1 = "10.0.0.1:1".parse::<SocketAddr>().unwrap();
        let addr2 = "10.0.0.2:1".parse::<SocketAddr>().unwrap();
        let node1 = runtime.create_node().ip(addr1.ip()).build();
        let node2 = runtime.create_node().ip(addr2.ip()).build();

        let listen_barrier = Arc::new(Barrier::new(2));
        let listen_barrier_ = listen_barrier.clone();

        node1.spawn(async move {
            let listener = TcpListener::bind(addr1).await.unwrap();
            listen_barrier.wait().await;

            let (mut socket, _) = listener.accept().await.unwrap();

            let mut read_buf = BytesMut::with_capacity(16);
            socket.read_buf(&mut read_buf).await.unwrap();

            assert_eq!(read_buf[0], 42);

            let mut write_buf = BytesMut::with_capacity(16);
            write_buf.put_u8(77);
            socket.write_all_buf(&mut write_buf).await.unwrap();
        });

        let f = node2.spawn(async move {
            listen_barrier_.wait().await;

            let mut socket = TcpStream::connect(addr1).await.unwrap();
            let mut write_buf = BytesMut::with_capacity(16);
            write_buf.put_u8(42);
            socket.write_all_buf(&mut write_buf).await.unwrap();

            let mut read_buf = BytesMut::with_capacity(16);
            socket.read_buf(&mut read_buf).await.unwrap();

            assert_eq!(read_buf[0], 77);
        });

        runtime.block_on(f).unwrap();
    }

    #[test]
    fn tcp_stream() {
        let runtime = Runtime::new();
        let addr1 = "10.0.0.1:1".parse::<SocketAddr>().unwrap();
        let addr2 = "10.0.0.2:1".parse::<SocketAddr>().unwrap();
        let node1 = runtime.create_node().ip(addr1.ip()).build();
        let node2 = runtime.create_node().ip(addr2.ip()).build();

        let listen_barrier = Arc::new(Barrier::new(2));
        let listen_barrier_ = listen_barrier.clone();

        let join_barrier = Arc::new(Barrier::new(2));
        let join_barrier_ = join_barrier.clone();

        node1.spawn(async move {
            let listener = TcpListener::bind(addr1).await.unwrap();
            listen_barrier.wait().await;

            let (socket, _) = listener.accept().await.unwrap();

            let (read, write) = socket.into_split();

            let read_fut = test_stream_read(read);
            let write_fut = test_stream_write(write);

            join!(read_fut, write_fut);

            join_barrier.wait().await;
        });

        let f = node2.spawn(async move {
            listen_barrier_.wait().await;

            let socket = TcpStream::connect(addr1).await.unwrap();
            let (read, write) = socket.into_split();

            let read_fut = test_stream_read(read);
            let write_fut = test_stream_write(write);

            join!(read_fut, write_fut);

            join_barrier_.wait().await;
        });

        runtime.block_on(f).unwrap();
    }
}
