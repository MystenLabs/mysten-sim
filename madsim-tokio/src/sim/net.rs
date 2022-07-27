use log::trace;

use std::{
    io,
    future::Future,
    net::{SocketAddr, ToSocketAddrs},
    pin::Pin,
    os::unix::io::{FromRawFd, IntoRawFd, AsRawFd, RawFd},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    time::Duration,
};

use madsim::net::{network::Payload, Endpoint};

use real_tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub use super::udp::*;
pub use super::unix::*;

use crate::poller::Poller;

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
        Self::bind_endpoint(Arc::new(Endpoint::bind(addr).await?))
    }

    fn bind_endpoint(ep: Arc<Endpoint>) -> io::Result<Self> {
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

    pub fn from_std(_listener: std::net::TcpListener) -> io::Result<TcpListener> {
        unimplemented!("from_std not supported in simulator")
    }

    pub fn into_std(self) -> io::Result<std::net::TcpListener> {
        unimplemented!("into_std not supported in simulator")
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.ep.local_addr()
    }

    pub fn ttl(&self) -> io::Result<u32> {
        unimplemented!("ttl not supported in simulator")
    }

    pub fn set_ttl(&self, _ttl: u32) -> io::Result<()> {
        unimplemented!("set_ttl not supported in simulator")
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

    fn read(&mut self, is_poll: bool, read: &mut ReadBuf<'_>) -> usize {
        let remaining_bytes = self.remaining_bytes();
        let to_write = std::cmp::min(remaining_bytes, read.remaining());
        if to_write > 0 {
            read.put_slice(&self.buffer[self.buffer_cursor..(self.buffer_cursor + to_write)]);
        }

        if !is_poll {
            self.buffer_cursor += to_write;

            if self.remaining_bytes() == 0 {
                self.buffer_cursor = 0;
                self.buffer.clear();
            }
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
            Ok(stream) => Ok(stream),
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

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.state.ep.peer_addr()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.state.ep.local_addr()
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

    fn poll_read_priv(&self, is_poll: bool, cx: &mut Context<'_>, read: &mut ReadBuf<'_>) -> Poll<io::Result<usize>> {
        debug_assert_ne!(read.remaining(), 0);

        let mut buffer = self.buffer.lock().unwrap();

        let num_bytes = buffer.read(is_poll, read);
        if num_bytes > 0 {
            // We might be able to read more from the network right now,
            // but for simplicity we just return immediately if there was anything in the buffer.
            return Poll::Ready(Ok(num_bytes));
        }

        let poll = self
            .read_poller
            .poll_with_fut(cx, || Self::read(self.state.clone()));

        match poll {
            Poll::Ready(Ok(mut buf)) => {
                // fill read with whatever we can, and save remainder for later.
                let num_bytes = buf.read(is_poll, read);
                buffer.write(buf);
                Poll::Ready(Ok(num_bytes))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    pub fn poll_peek(
        &self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<usize>> {
        self.poll_read_priv(true, cx, buf)
    }

    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.len() == 0 {
            return Ok(0);
        }

        struct Fut<'a> {
            buf: ReadBuf<'a>,
            stream: &'a TcpStream,
        }
        impl<'a> Future for Fut<'a> {
            type Output = io::Result<usize>;
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                self.stream.poll_peek(cx, &mut self.buf)
            }
        }

        let mut buf = ReadBuf::new(buf);
        buf.clear();

        let fut = Fut { stream: self, buf };
        fut.await
    }

    // flush and shutdown are no-ops
    fn poll_flush_priv(&self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown_priv(&self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        // TODO: implement?
        Poll::Ready(Ok(()))
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        todo!()
    }

    pub fn set_nodelay(&self, _nodelay: bool) -> io::Result<()> {
        todo!()
    }

    pub fn linger(&self) -> io::Result<Option<Duration>> {
        todo!()
    }

    pub fn set_linger(&self, _dur: Option<Duration>) -> io::Result<()> {
        todo!()
    }

    pub fn ttl(&self) -> io::Result<u32> {
        todo!()
    }

    pub fn set_ttl(&self, _ttl: u32) -> io::Result<()> {
        todo!()
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        read: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.poll_read_priv(false, cx, read).map(|r| r.map(|_| ()))
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

impl OwnedReadHalf {
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.peek(buf).await
    }
}

impl AsyncRead for OwnedReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        read: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.inner.poll_read_priv(false, cx, read).map(|r| r.map(|_| ()))
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

pub struct TcpSocket {
    bind_addr: Mutex<Option<Endpoint>>,
}

impl TcpSocket {
    // TODO: simulate v4/v6?
    pub fn new_v4() -> io::Result<TcpSocket> {
        TcpSocket::new()
    }

    pub fn new_v6() -> io::Result<TcpSocket> {
        TcpSocket::new()
    }

    fn new() -> io::Result<TcpSocket> {
        Ok(TcpSocket {
            bind_addr: Mutex::new(None),
        })
    }

    pub fn set_reuseaddr(&self, _reuseaddr: bool) -> io::Result<()> {
        todo!()
    }

    pub fn reuseaddr(&self) -> io::Result<bool> {
        todo!()
    }

    pub fn set_reuseport(&self, _reuseport: bool) -> io::Result<()> {
        todo!()
    }

    pub fn reuseport(&self) -> io::Result<bool> {
        todo!()
    }

    pub fn set_send_buffer_size(&self, _size: u32) -> io::Result<()> {
        todo!()
    }

    pub fn send_buffer_size(&self) -> io::Result<u32> {
        todo!()
    }

    pub fn set_recv_buffer_size(&self, _size: u32) -> io::Result<()> {
        todo!()
    }

    pub fn recv_buffer_size(&self) -> io::Result<u32> {
        todo!()
    }

    pub fn set_linger(&self, _dur: Option<Duration>) -> io::Result<()> {
        todo!()
    }

    pub fn linger(&self) -> io::Result<Option<Duration>> {
        todo!()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.bind_addr
            .lock()
            .unwrap()
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "socket is not connected"))
            .map(|ep| ep.local_addr().unwrap())
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        todo!()
    }

    pub fn bind(&self, addr: SocketAddr) -> io::Result<()> {
        let ep = Endpoint::bind_sync(addr)?;
        *self.bind_addr.lock().unwrap() = Some(ep);
        Ok(())
    }

    pub async fn connect(self, addr: SocketAddr) -> io::Result<TcpStream> {
        TcpStream::connect(addr).await
    }

    pub fn listen(self, _backlog: u32) -> io::Result<TcpListener> {
        let ep = Arc::new(self.bind_addr.into_inner().unwrap().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "socket is not connected")
        })?);
        TcpListener::bind_endpoint(ep)
    }

    pub fn from_std_stream(_std_stream: std::net::TcpStream) -> TcpSocket {
        unimplemented!("from_std_stream not supported in simulator")
    }
}

impl AsRawFd for TcpSocket {
    fn as_raw_fd(&self) -> RawFd {
        unimplemented!("as_raw_fd not supported in simulator")
    }
}

impl FromRawFd for TcpSocket {
    unsafe fn from_raw_fd(_fd: RawFd) -> TcpSocket {
        unimplemented!("from_raw_fd not supported in simulator")
    }
}

impl IntoRawFd for TcpSocket {
    fn into_raw_fd(self) -> RawFd {
        unimplemented!("into_raw_fd not supported in simulator")
    }
}

impl AsRawFd for TcpStream {
    fn as_raw_fd(&self) -> RawFd {
        unimplemented!("as_raw_fd not supported in simulator")
    }
}

impl FromRawFd for TcpStream {
    unsafe fn from_raw_fd(_fd: RawFd) -> TcpStream {
        unimplemented!("from_raw_fd not supported in simulator")
    }
}

impl IntoRawFd for TcpStream {
    fn into_raw_fd(self) -> RawFd {
        unimplemented!("into_raw_fd not supported in simulator")
    }
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
            let peek_size = (rand.next_u32() % 0xff).try_into().unwrap();
            let mut peek_buf = vec![0u8; peek_size];
            let n = stream.peek(&mut peek_buf).await.unwrap();
            let mut peek_next_read = next_read;
            for byte in &peek_buf[0..n] {
                if *byte == 0xff {
                    break;
                }
                assert_eq!(*byte, peek_next_read);
                peek_next_read += 1;
                peek_next_read %= 0xfe;
            }


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
