use tracing::{debug, trace};

use std::{
    future::Future,
    io,
    net::SocketAddr,
    os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    time::Duration,
};

pub use std::net::ToSocketAddrs;

use msim::net::{
    get_endpoint_from_socket,
    network::{Payload, PayloadType},
    try_get_endpoint_from_socket, Endpoint, OwnedFd,
};
use real_tokio::io::{AsyncRead, AsyncWrite, Interest, ReadBuf, Ready};

pub use super::udp::*;
pub use super::unix;
pub use super::unix::*;

use crate::poller::Poller;

/// Provide the tokio::net::TcpListener interface.
pub struct TcpListener {
    fd: OwnedFd,
    ep: Arc<Endpoint>,
    poller: Poller<io::Result<(TcpStream, SocketAddr)>>,
}

impl std::fmt::Debug for TcpListener {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        fmt.debug_struct("TcpListener")
            .field("fd", &self.fd)
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
        let tcp_sock = std::net::TcpListener::bind(addr)?;
        Self::from_std(tcp_sock)
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

        let remote_tcp_id = Message::new(msg).unwrap_tcp_id();
        trace!(
            "server: read remote tcp id from client {} {}",
            from,
            remote_tcp_id
        );

        let local_tcp_id: u32 = ep.allocate_local_tcp_id();

        let state = TcpState::new(ep, local_tcp_id, remote_tcp_id, from);

        // tell other side what our tcp id is
        trace!(
            "server: sending local_tcp_id {} to client {}-{}",
            local_tcp_id,
            from,
            remote_tcp_id
        );

        state
            .ep
            .send_to_raw(from, state.next_send_tag(), Message::tcp_id(local_tcp_id))
            .await
            .map_err(|e| {
                trace!("error sending local_tcp_id to {}: {} ", from, e);
                io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    format!("peer {} hung up", from),
                )
            })?;

        debug!(
            "accepted new tcp connection from {} -> {}",
            state.remote_sock,
            state.ep.local_addr().unwrap(),
        );
        let stream = TcpStream::new(state);
        Ok((stream, from))
    }

    pub fn from_std(listener: std::net::TcpListener) -> io::Result<TcpListener> {
        let fd: OwnedFd = listener.into_raw_fd().into();
        let ep = get_endpoint_from_socket(fd.as_raw_fd())?;
        Ok(Self {
            fd,
            ep,
            poller: Poller::new(),
        })
    }

    pub fn into_std(self) -> io::Result<std::net::TcpListener> {
        let Self { fd, .. } = self;
        unsafe { Ok(std::net::TcpListener::from_raw_fd(fd.release())) }
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

#[derive(Debug)]
enum Message {
    TcpId(u32),
    // sequence number + payload - the simulator is unordered.
    // sequence number is unnecessary, but we use it for a debug assert to make sure we haven't
    // screwed up the sequencing.
    Payload(u32, Vec<u8>),
}

impl Message {
    fn new(payload: Payload) -> Self {
        let s = *payload.data.downcast::<Message>().unwrap();
        match (&s, &payload.ty) {
            (Message::TcpId(_), PayloadType::TcpSignalConnect)
            | (Message::Payload(_, _), PayloadType::TcpData) => (),
            _ => panic!("invalid payload type {:?}, {:?}", s, payload.ty),
        }
        s
    }

    fn tcp_id(p: u32) -> Payload {
        Payload::new_tcp_connect(Box::new(Message::TcpId(p)))
    }

    fn payload(s: u32, v: Vec<u8>) -> Payload {
        Payload::new_tcp_data(Box::new(Message::Payload(s, v)))
    }

    fn unwrap_payload(self) -> (u32, Vec<u8>) {
        match self {
            Message::Payload(s, v) => (s, v),
            _ => panic!("expected payload"),
        }
    }

    fn unwrap_tcp_id(self) -> u32 {
        match self {
            Message::TcpId(p) => p,
            _ => panic!("expected TcpId"),
        }
    }
}

#[derive(Debug)]
struct TcpState {
    ep: Arc<Endpoint>,
    send_seq: AtomicU32,
    recv_seq: AtomicU32,
    local_tcp_id: u32,
    remote_tcp_id: u32,
    remote_sock: SocketAddr,

    // not simulated, only present to return the correct value with getters/settters.
    nodelay: AtomicBool,
}

impl TcpState {
    fn new(
        ep: Arc<Endpoint>,
        local_tcp_id: u32,
        remote_tcp_id: u32,
        remote_sock: SocketAddr,
    ) -> Self {
        Self {
            ep,
            send_seq: AtomicU32::new(0),
            recv_seq: AtomicU32::new(0),
            local_tcp_id,
            remote_tcp_id,
            remote_sock,
            nodelay: AtomicBool::new(false),
        }
    }

    fn next_send_tag(&self) -> u64 {
        let seq = self.send_seq.fetch_add(1, Ordering::SeqCst) as u64;
        let tcp_id = self.remote_tcp_id as u64;
        (tcp_id << 32) | seq
    }

    fn next_recv_tag(&self) -> u64 {
        let seq = self.recv_seq.load(Ordering::SeqCst) as u64;
        let tcp_id = self.local_tcp_id as u64;
        (tcp_id << 32) | seq
    }

    fn increment_recv_tag(&self) {
        self.recv_seq.fetch_add(1, Ordering::SeqCst);
    }

    fn get_next_recv_tag_and_inc(&self) -> u64 {
        let ret = self.next_recv_tag();
        self.increment_recv_tag();
        ret
    }
}

impl Drop for TcpState {
    fn drop(&mut self) {
        self.ep
            .deregister_tcp_id(&self.remote_sock, self.local_tcp_id);
    }
}

/// Provide the tokio::net::TcpStream interface.
pub struct TcpStream {
    state: Arc<TcpState>,
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
        let local_tcp_id = ep.allocate_local_tcp_id();

        // partially initialized state
        let mut state = TcpState::new(ep, local_tcp_id, 0xdeadbeef, remote_sock);

        // establish new connection, use reserved tag 0 to tell other side the local tcp_id.
        trace!(
            "sending server {} local tcp_id {}",
            remote_sock,
            local_tcp_id
        );
        state
            .ep
            .send_to_raw(remote_sock, 0, Message::tcp_id(state.local_tcp_id))
            .await?;

        // read the remote tcp_id back
        trace!(
            "awaiting remote tcp_id from server {} {}",
            remote_sock,
            local_tcp_id
        );

        let (msg, from) = state
            .ep
            .recv_from_raw(state.get_next_recv_tag_and_inc())
            .await?;
        debug_assert_eq!(from, remote_sock);
        // finish initializing state
        state.remote_tcp_id = Message::new(msg).unwrap_tcp_id();
        trace!(
            "received remote tcp_id from server {} <- {}",
            state.remote_sock,
            from
        );
        debug!(
            "new TcpStream connection to {} (id: {})",
            state.remote_sock, local_tcp_id
        );
        Ok(Self::new(state))
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.state.ep.peer_addr()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.state.ep.local_addr()
    }

    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        split_owned(self)
    }

    fn poll_write_priv(&self, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let num = buf.len();
        let tag = self.state.next_send_tag();
        let seq = (tag & 0xffffffff) as u32;
        match self.state.ep.send_to_raw_sync(
            self.state.remote_sock,
            tag,
            Message::payload(seq, buf.to_vec()),
        ) {
            Err(err) => Poll::Ready(Err(err)),
            Ok(_) => Poll::Ready(Ok(num)),
        }
    }

    fn poll_read_priv(
        &self,
        is_poll: bool,
        cx: &mut Context<'_>,
        read: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<usize>> {
        debug_assert_ne!(read.remaining(), 0);

        let mut buffer = self.buffer.lock().unwrap();

        let num_bytes = buffer.read(is_poll, read);
        if num_bytes > 0 {
            // We might be able to read more from the network right now,
            // but for simplicity we just return immediately if there was anything in the buffer.
            return Poll::Ready(Ok(num_bytes));
        }

        let remote_tcp_id = self.state.remote_tcp_id;

        // we learn about closed connections instantly. this is not realistic, but is good enough
        // for most purposes.
        if !self
            .state
            .ep
            .is_peer_live(Some(self.state.remote_sock), remote_tcp_id)
        {
            debug!("peer {} hung up", self.state.remote_sock);
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                format!("peer {} hung up", self.state.remote_sock),
            )));
        }

        let tag = self.state.next_recv_tag();

        match self.state.ep.recv_ready(Some(cx), tag) {
            Err(err) => Err(err).into(),
            Ok(false) => Poll::Pending,
            Ok(true) => {
                let (payload, from) = match self.state.ep.recv_from_raw_sync(tag) {
                    Err(err) => return Poll::Ready(Err(err)),
                    Ok(r) => r,
                };
                self.state.increment_recv_tag();
                debug_assert_eq!(from, self.state.remote_sock);

                let (seq, payload) = Message::new(payload).unwrap_payload();
                debug_assert_eq!(seq as u64, tag & 0xffffffff);
                let mut buf = Buffer::new_from_vec(payload);
                let num_bytes = buf.read(is_poll, read);
                buffer.write(buf);
                Poll::Ready(Ok(num_bytes))
            }
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

    pub async fn ready(&self, _interest: Interest) -> io::Result<Ready> {
        todo!()
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
        Ok(self.state.nodelay.load(Ordering::SeqCst))
    }

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.state.nodelay.store(nodelay, Ordering::SeqCst);
        Ok(())
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

    pub fn from_std(stream: std::net::TcpStream) -> io::Result<TcpStream> {
        let fd = stream.into_raw_fd();
        unsafe { Ok(Self::from_raw_fd(fd)) }
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
        self.inner
            .poll_read_priv(false, cx, read)
            .map(|r| r.map(|_| ()))
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
    fd: OwnedFd,
    bind_addr: Mutex<Option<Arc<Endpoint>>>,
}

impl TcpSocket {
    // TODO: simulate v4/v6?
    pub fn new_v4() -> io::Result<TcpSocket> {
        let tcp_sock = real_tokio::net::TcpSocket::new_v4()?;
        Ok(unsafe { Self::from_raw_fd(tcp_sock.into_raw_fd()) })
    }

    pub fn new_v6() -> io::Result<TcpSocket> {
        unimplemented!("ipv6 not supported in simulator");
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
        *self.bind_addr.lock().unwrap() = Some(ep.into());
        Ok(())
    }

    pub async fn connect(self, addr: SocketAddr) -> io::Result<TcpStream> {
        TcpStream::connect(addr).await
    }

    pub fn listen(self, backlog: u32) -> io::Result<TcpListener> {
        let sock = unsafe { real_tokio::net::TcpSocket::from_raw_fd(self.into_raw_fd()) };
        let listener = sock.listen(backlog)?;
        TcpListener::from_std(listener.into_std()?)
    }

    pub fn from_std_stream(_std_stream: std::net::TcpStream) -> TcpSocket {
        unimplemented!("from_std_stream not supported in simulator")
    }
}

impl AsRawFd for TcpSocket {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl FromRawFd for TcpSocket {
    unsafe fn from_raw_fd(fd: RawFd) -> TcpSocket {
        let ep = try_get_endpoint_from_socket(fd).expect("socket does not exist");
        TcpSocket {
            fd: fd.into(),
            bind_addr: Mutex::new(ep),
        }
    }
}

impl IntoRawFd for TcpSocket {
    fn into_raw_fd(self) -> RawFd {
        self.fd.release()
    }
}

// To support conversion between TcpStream <-> RawFd we will need to lower the TcpState
// and reading/writing operations to the net interceptor library - otherwise we can't track any
// reads/writes that occur while the stream is being manipulated as a raw fd.
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

pub async fn lookup_host<T>(host: T) -> io::Result<impl Iterator<Item = SocketAddr>>
where
    T: ToSocketAddrs,
{
    // pretend to be async in simulator
    msim::time::sleep(Duration::from_micros(10)).await;
    host.to_socket_addrs()
}

#[cfg(test)]
mod tests {

    use super::{OwnedReadHalf, OwnedWriteHalf, TcpListener, TcpStream};
    use bytes::{BufMut, BytesMut};
    use futures::join;
    use msim::{
        rand,
        rand::RngCore,
        runtime::{init_logger, Handle, Runtime},
        time::{sleep, timeout, Duration},
    };
    use real_tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::Barrier,
    };
    use std::{io, net::SocketAddr, sync::Arc};
    use tracing::{debug, trace};

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
        runtime.block_on(async move {
            let addr1 = "10.0.0.1:1".parse::<SocketAddr>().unwrap();
            let addr2 = "10.0.0.2:1".parse::<SocketAddr>().unwrap();
            let handle = Handle::current();
            let node1 = handle.create_node().ip(addr1.ip()).build();
            let node2 = handle.create_node().ip(addr2.ip()).build();

            let listen_barrier = Arc::new(Barrier::new(2));
            let listen_barrier_ = listen_barrier.clone();

            let join_barrier = Arc::new(Barrier::new(2));
            let join_barrier_ = join_barrier.clone();

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

                join_barrier.wait().await;
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

                join_barrier_.wait().await;
            });

            f.await.unwrap();
        });
    }

    #[test]
    fn tcp_stream() {
        init_logger();
        let runtime = Runtime::new();
        runtime.block_on(async move {
            let addr1 = "10.0.0.1:1".parse::<SocketAddr>().unwrap();
            let addr2 = "10.0.0.2:1".parse::<SocketAddr>().unwrap();
            let handle = Handle::current();
            let node1 = handle.create_node().ip(addr1.ip()).build();
            let node2 = handle.create_node().ip(addr2.ip()).build();

            let listen_barrier = Arc::new(Barrier::new(2));
            let listen_barrier_ = listen_barrier.clone();

            let join_barrier = Arc::new(Barrier::new(2));
            let join_barrier_ = join_barrier.clone();

            node1.spawn(async move {
                let listener = TcpListener::bind(addr1).await.unwrap();

                // forget the first listener and rebind to make sure that ports are released and
                // rebindable
                std::mem::drop(listener);
                let listener = TcpListener::bind(addr1).await.unwrap();

                listen_barrier.wait().await;

                let (socket, _) = listener.accept().await.unwrap();

                let (read, write) = socket.into_split();

                // keep the underlying stream alive until after the read and write futures have
                // joined, or else the other end will fail with a ConnectionReset error.
                let _stream = read.inner.clone();

                let read_fut = test_stream_read(read);
                let write_fut = test_stream_write(write);

                join!(read_fut, write_fut);

                join_barrier.wait().await;
            });

            let f = node2.spawn(async move {
                listen_barrier_.wait().await;

                let socket = TcpStream::connect(addr1).await.unwrap();
                let (read, write) = socket.into_split();

                // keep the underlying stream alive until after the read and write futures have
                // joined, or else the other end will fail with a ConnectionReset error.
                let _stream = read.inner.clone();

                let read_fut = test_stream_read(read);
                let write_fut = test_stream_write(write);

                join!(read_fut, write_fut);

                join_barrier_.wait().await;
            });

            f.await.unwrap();
        });
    }

    #[test]
    fn tcp_test_failed_connect() {
        init_logger();
        let runtime = Runtime::new();

        runtime.block_on(async move {
            let addr1 = "10.0.0.1:1".parse::<SocketAddr>().unwrap();
            let addr2 = "10.0.0.2:1".parse::<SocketAddr>().unwrap();
            let handle = Handle::current();
            let node1 = handle.create_node().ip(addr1.ip()).build();
            let node2 = handle.create_node().ip(addr2.ip()).build();

            let join_barrier = Arc::new(Barrier::new(2));
            let join_barrier_ = join_barrier.clone();

            node1.spawn(async move {
                join_barrier.wait().await;
            });

            let f = node2.spawn(async move {
                let err = TcpStream::connect(addr1).await.unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
                join_barrier_.wait().await;
            });

            f.await.unwrap();
        });
    }

    #[test]
    fn tcp_test_server_hangup() {
        init_logger();
        let runtime = Runtime::new();

        runtime.block_on(async move {
            let addr1 = "10.0.0.1:1".parse::<SocketAddr>().unwrap();
            let addr2 = "10.0.0.2:1".parse::<SocketAddr>().unwrap();
            let handle = Handle::current();
            let node1 = handle.create_node().ip(addr1.ip()).build();
            let node2 = handle.create_node().ip(addr2.ip()).build();

            let listen_barrier = Arc::new(Barrier::new(2));
            let listen_barrier_ = listen_barrier.clone();

            let join_barrier = Arc::new(Barrier::new(2));
            let join_barrier_ = join_barrier.clone();

            node1.spawn(async move {
                debug!("server begin");
                let listener = TcpListener::bind(addr1).await.unwrap();
                listen_barrier.wait().await;

                let (mut socket, _) = listener.accept().await.unwrap();

                timeout(Duration::from_secs(1), async move {
                    loop {
                        let next = socket.read_u64().await.unwrap();
                        trace!("read {}", next);
                    }
                })
                .await
                .unwrap_err();
                debug!("server exited");

                join_barrier.wait().await;
            });

            let f = node2.spawn(async move {
                listen_barrier_.wait().await;

                let mut socket = TcpStream::connect(addr1).await.unwrap();

                timeout(Duration::from_secs(2), async move {
                    debug!("client begun");
                    for i in 0.. {
                        trace!("writing {}", i);
                        if let Err(err) = socket.write_u64(i).await {
                            debug!("client err {}", err);
                            assert_eq!(err.kind(), io::ErrorKind::ConnectionReset);
                            break;
                        }
                        sleep(Duration::from_millis(100)).await;
                    }
                    debug!("client exited");
                })
                .await
                .expect("write loop should have exited due to ConnectionReset");

                join_barrier_.wait().await;
            });

            f.await.unwrap();
        });
    }

    #[test]
    fn tcp_test_client_hangup() {
        init_logger();
        let runtime = Runtime::new();

        runtime.block_on(async move {
            let addr1 = "10.0.0.1:1".parse::<SocketAddr>().unwrap();
            let addr2 = "10.0.0.2:1".parse::<SocketAddr>().unwrap();
            let handle = Handle::current();
            let node1 = handle.create_node().ip(addr1.ip()).build();
            let node2 = handle.create_node().ip(addr2.ip()).build();

            let listen_barrier = Arc::new(Barrier::new(2));
            let listen_barrier_ = listen_barrier.clone();

            let join_barrier = Arc::new(Barrier::new(2));
            let join_barrier_ = join_barrier.clone();

            node1.spawn(async move {
                debug!("server begin");
                let listener = TcpListener::bind(addr1).await.unwrap();
                listen_barrier.wait().await;

                let (mut socket, _) = listener.accept().await.unwrap();

                timeout(Duration::from_secs(2), async move {
                    debug!("server write begun");
                    for i in 0.. {
                        trace!("writing {}", i);
                        if let Err(err) = socket.write_u64(i).await {
                            assert_eq!(err.kind(), io::ErrorKind::ConnectionReset);
                            break;
                        }
                        sleep(Duration::from_millis(100)).await;
                    }
                    debug!("client exited");
                })
                .await
                .expect("write loop should have exited due to ConnectionReset");

                debug!("server exited");

                join_barrier.wait().await;
            });

            let f = node2.spawn(async move {
                listen_barrier_.wait().await;

                let mut socket = TcpStream::connect(addr1).await.unwrap();

                timeout(Duration::from_secs(1), async move {
                    loop {
                        let next = socket.read_u64().await.unwrap();
                        trace!("read {}", next);
                    }
                })
                .await
                .unwrap_err();

                join_barrier_.wait().await;
            });

            f.await.unwrap();
        });
    }

    #[test]
    fn tcp_test_client_hangup_read() {
        init_logger();
        let runtime = Runtime::new();

        runtime.block_on(async move {
            let addr1 = "10.0.0.1:1".parse::<SocketAddr>().unwrap();
            let addr2 = "10.0.0.2:1".parse::<SocketAddr>().unwrap();
            let handle = Handle::current();
            let node1 = handle.create_node().ip(addr1.ip()).build();
            let node2 = handle.create_node().ip(addr2.ip()).build();

            let listen_barrier = Arc::new(Barrier::new(2));
            let listen_barrier_ = listen_barrier.clone();

            let join_barrier = Arc::new(Barrier::new(2));
            let join_barrier_ = join_barrier.clone();

            node1.spawn(async move {
                debug!("server begin");
                let listener = TcpListener::bind(addr1).await.unwrap();
                listen_barrier.wait().await;

                let (socket, _) = listener.accept().await.unwrap();

                sleep(Duration::from_secs(1)).await;

                debug!("server finished");
                drop(socket);

                join_barrier.wait().await;
            });

            let f = node2.spawn(async move {
                listen_barrier_.wait().await;

                let mut socket = TcpStream::connect(addr1).await.unwrap();

                debug!("client connect");

                assert_eq!(
                    socket.read_u64().await.unwrap_err().kind(),
                    io::ErrorKind::ConnectionReset
                );

                debug!("client finished");

                join_barrier_.wait().await;
            });

            f.await.unwrap();
        });
    }

    #[test]
    fn tcp_test_server_hangup_read() {
        init_logger();
        let runtime = Runtime::new();

        runtime.block_on(async move {
            let addr1 = "10.0.0.1:1".parse::<SocketAddr>().unwrap();
            let addr2 = "10.0.0.2:1".parse::<SocketAddr>().unwrap();
            let handle = Handle::current();
            let node1 = handle.create_node().ip(addr1.ip()).build();
            let node2 = handle.create_node().ip(addr2.ip()).build();

            let listen_barrier = Arc::new(Barrier::new(2));
            let listen_barrier_ = listen_barrier.clone();

            let join_barrier = Arc::new(Barrier::new(2));
            let join_barrier_ = join_barrier.clone();

            node1.spawn(async move {
                debug!("server begin");
                let listener = TcpListener::bind(addr1).await.unwrap();
                listen_barrier.wait().await;

                let (mut socket, _) = listener.accept().await.unwrap();

                assert_eq!(
                    socket.read_u64().await.unwrap_err().kind(),
                    io::ErrorKind::ConnectionReset
                );

                debug!("server finished");

                join_barrier.wait().await;
            });

            let f = node2.spawn(async move {
                listen_barrier_.wait().await;

                let socket = TcpStream::connect(addr1).await.unwrap();

                debug!("client connect");

                sleep(Duration::from_secs(1)).await;

                debug!("client finished");
                drop(socket);

                join_barrier_.wait().await;
            });

            f.await.unwrap();
        });
    }
}
