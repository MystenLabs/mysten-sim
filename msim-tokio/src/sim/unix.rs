#![allow(dead_code)]
#![allow(unused_variables)]

use std::{
    io,
    net::Shutdown,
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
