#![allow(dead_code)]
#![allow(unused_variables)]

use bytes::BufMut;
use std::{
    io,
    net::Shutdown,
    os::unix::{
        io::{FromRawFd, RawFd},
        net,
    },
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};

use real_tokio::io::{AsyncRead, AsyncWrite, Interest, ReadBuf, Ready};

pub use real_tokio::net::unix::UCred;
pub use std::net::SocketAddr;

/// Provide the tokio::net::UnixListener interface.
#[derive(Debug)]
pub struct UnixListener {}

impl UnixListener {
    /// todo
    pub fn bind<P>(path: P) -> io::Result<UnixListener>
    where
        P: AsRef<Path>,
    {
        todo!()
    }

    /// todo
    pub fn from_std(listener: UnixListener) -> io::Result<UnixListener> {
        todo!()
    }

    /// todo
    pub fn into_std(self) -> io::Result<std::os::unix::net::UnixListener> {
        todo!()
    }

    /// todo
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        todo!()
    }

    /// todo
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        todo!()
    }

    /// todo
    pub async fn accept(&self) -> io::Result<(UnixStream, SocketAddr)> {
        todo!()
    }

    /// todo
    pub fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<io::Result<(UnixStream, SocketAddr)>> {
        todo!()
    }
}

/// Provide the tokio::net::UnixStream interface.
#[derive(Debug)]
pub struct UnixStream {}

impl UnixStream {
    /// todo
    pub async fn connect<P>(path: P) -> io::Result<UnixStream>
    where
        P: AsRef<Path>,
    {
        todo!()
    }

    /// todo
    pub async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        todo!()
    }

    /// todo
    pub async fn readable(&self) -> io::Result<()> {
        todo!()
    }

    /// todo
    pub fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    /// todo
    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        todo!()
    }

    /// todo
    pub fn try_read_vectored(&self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        todo!()
    }

    /*
    cfg_io_util! {
        /// todo
        pub fn try_read_buf<B: BufMut>(&self, buf: &mut B) -> io::Result<usize> {
            todo!()
        }
    }
    */

    /// todo
    pub async fn writable(&self) -> io::Result<()> {
        todo!()
    }

    /// todo
    pub fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    /// todo
    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        todo!()
    }

    /// todo
    pub fn try_write_vectored(&self, buf: &[io::IoSlice<'_>]) -> io::Result<usize> {
        todo!()
    }

    /// todo
    pub fn try_io<R>(
        &self,
        interest: Interest,
        f: impl FnOnce() -> io::Result<R>,
    ) -> io::Result<R> {
        todo!()
    }

    /// todo
    pub fn from_std(stream: UnixStream) -> io::Result<UnixStream> {
        todo!()
    }

    /// todo
    pub fn into_std(self) -> io::Result<std::os::unix::net::UnixStream> {
        todo!()
    }

    /// todo
    pub fn pair() -> io::Result<(UnixStream, UnixStream)> {
        todo!()
    }

    /// todo
    pub(crate) fn new(stream: UnixStream) -> io::Result<UnixStream> {
        todo!()
    }

    /// todo
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        todo!()
    }

    /// todo
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        todo!()
    }

    /// todo
    pub fn peer_cred(&self) -> io::Result<UCred> {
        todo!()
    }

    /// todo
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        todo!()
    }

    /// todo
    pub(super) fn shutdown_std(&self, how: Shutdown) -> io::Result<()> {
        todo!()
    }

    // These lifetime markers also appear in the generated documentation, and make
    // it more clear that this is a *borrowed* split.
    #[allow(clippy::needless_lifetimes)]
    /// todo
    pub fn split<'a>(&'a mut self) -> (ReadHalf<'a>, WriteHalf<'a>) {
        todo!()
    }

    /// todo
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        todo!()
    }

    fn poll_read_priv(&self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    fn poll_write_priv(&self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        todo!()
    }
}

impl FromRawFd for UnixStream {
    unsafe fn from_raw_fd(fd: RawFd) -> UnixStream {
        todo!()
    }
}

impl AsyncRead for UnixStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        read: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.poll_read_priv(cx, read)
    }
}

impl AsyncWrite for UnixStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_priv(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shutdown_std(std::net::Shutdown::Write)?;
        Poll::Ready(Ok(()))
    }
}

pub struct ReadHalf<'a>(&'a std::marker::PhantomData<u8>);
pub struct WriteHalf<'a>(&'a std::marker::PhantomData<u8>);
pub struct OwnedReadHalf;
pub struct OwnedWriteHalf;

pub struct UnixDatagram;

impl UnixDatagram {
    pub async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        unimplemented!()
    }

    pub async fn writable(&self) -> io::Result<()> {
        unimplemented!()
    }

    pub fn poll_send_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        unimplemented!()
    }

    pub async fn readable(&self) -> io::Result<()> {
        unimplemented!()
    }

    pub fn poll_recv_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        unimplemented!()
    }

    pub fn bind<P>(path: P) -> io::Result<UnixDatagram>
    where
        P: AsRef<Path>,
    {
        unimplemented!()
    }

    pub fn pair() -> io::Result<(UnixDatagram, UnixDatagram)> {
        unimplemented!()
    }

    pub fn from_std(datagram: net::UnixDatagram) -> io::Result<UnixDatagram> {
        unimplemented!()
    }

    pub fn into_std(self) -> io::Result<std::os::unix::net::UnixDatagram> {
        unimplemented!()
    }

    fn new(socket: mio::net::UnixDatagram) -> io::Result<UnixDatagram> {
        unimplemented!()
    }

    pub fn unbound() -> io::Result<UnixDatagram> {
        unimplemented!()
    }

    pub fn connect<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        unimplemented!()
    }

    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        unimplemented!()
    }

    pub fn try_send(&self, buf: &[u8]) -> io::Result<usize> {
        unimplemented!()
    }

    pub fn try_send_to<P>(&self, buf: &[u8], target: P) -> io::Result<usize>
    where
        P: AsRef<Path>,
    {
        unimplemented!()
    }

    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        unimplemented!()
    }

    pub fn try_recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        unimplemented!()
    }

    pub fn try_recv_buf_from<B: BufMut>(&self, buf: &mut B) -> io::Result<(usize, SocketAddr)> {
        unimplemented!()
    }

    pub fn try_recv_buf<B: BufMut>(&self, buf: &mut B) -> io::Result<usize> {
        unimplemented!()
    }

    pub async fn send_to<P>(&self, buf: &[u8], target: P) -> io::Result<usize>
    where
        P: AsRef<Path>,
    {
        unimplemented!()
    }

    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        unimplemented!()
    }

    pub fn poll_recv_from(
        &self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<SocketAddr>> {
        unimplemented!()
    }

    pub fn poll_send_to<P>(
        &self,
        cx: &mut Context<'_>,
        buf: &[u8],
        target: P,
    ) -> Poll<io::Result<usize>>
    where
        P: AsRef<Path>,
    {
        unimplemented!()
    }

    pub fn poll_send(&self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        unimplemented!()
    }

    pub fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        unimplemented!()
    }

    pub fn try_recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        unimplemented!()
    }

    pub fn try_io<R>(
        &self,
        interest: Interest,
        f: impl FnOnce() -> io::Result<R>,
    ) -> io::Result<R> {
        unimplemented!()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        unimplemented!()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        unimplemented!()
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        unimplemented!()
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        unimplemented!()
    }
}
