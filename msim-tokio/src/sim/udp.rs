use std::{
    io, net,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use msim::net::Endpoint;
use real_tokio::io::{Interest, ReadBuf, Ready};

use bytes::BufMut;

#[derive(Debug)]
pub struct UdpSocket {
    ep: Arc<Endpoint>,
    default_dest: Mutex<Option<SocketAddr>>,
}

impl UdpSocket {
    fn new(ep: Arc<Endpoint>) -> Self {
        Self {
            ep,
            default_dest: Mutex::new(None),
        }
    }

    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpSocket> {
        let addrs = addr.to_socket_addrs()?;

        let mut last_err = None;

        for addr in addrs {
            match Self::bind_addr(addr) {
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

    fn bind_addr(addr: SocketAddr) -> io::Result<UdpSocket> {
        let ep = Arc::new(Endpoint::bind_sync(addr)?);
        Ok(Self::new(ep))
    }

    pub fn from_std(_socket: net::UdpSocket) -> io::Result<UdpSocket> {
        unimplemented!("cannot create udp socket from net::UdpSocket")
    }

    pub fn into_std(self) -> io::Result<std::net::UdpSocket> {
        unimplemented!("cannot unwrap udp socket into net::UdpSocket")
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.ep.local_addr()?)
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.ep.peer_addr()?)
    }

    pub async fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        let mut addrs = addr.to_socket_addrs()?;
        // for UDP, connection is just setting the default destination
        *self.default_dest.lock().unwrap() = Some(addrs.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        })?);
        Ok(())
    }

    pub async fn ready(&self, _interest: Interest) -> io::Result<Ready> {
        todo!()
    }

    pub async fn writable(&self) -> io::Result<()> {
        self.ready(Interest::WRITABLE).await?;
        Ok(())
    }

    pub fn poll_send_ready(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    pub async fn send(&self, _buf: &[u8]) -> io::Result<usize> {
        todo!()
    }

    pub fn poll_send(&self, _cx: &mut Context<'_>, _buf: &[u8]) -> Poll<io::Result<usize>> {
        todo!()
    }

    pub fn try_send(&self, _buf: &[u8]) -> io::Result<usize> {
        todo!()
    }

    pub async fn readable(&self) -> io::Result<()> {
        self.ready(Interest::READABLE).await?;
        Ok(())
    }

    pub fn poll_recv_ready(&self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    pub async fn recv(&self, _buf: &mut [u8]) -> io::Result<usize> {
        todo!()
    }

    pub fn poll_recv(&self, _cx: &mut Context<'_>, _buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    pub fn try_recv(&self, _buf: &mut [u8]) -> io::Result<usize> {
        todo!()
    }

    pub fn try_recv_buf<B: BufMut>(&self, _buf: &mut B) -> io::Result<usize> {
        todo!()
    }

    pub fn try_recv_buf_from<B: BufMut>(&self, _buf: &mut B) -> io::Result<(usize, SocketAddr)> {
        todo!()
    }

    pub async fn send_to<A: ToSocketAddrs>(&self, buf: &[u8], target: A) -> io::Result<usize> {
        let mut addrs = target.to_socket_addrs()?;

        match addrs.next() {
            Some(target) => self.send_to_addr(buf, target).await,
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no addresses to send data to",
            )),
        }
    }

    pub fn poll_send_to(
        &self,
        _cx: &mut Context<'_>,
        _buf: &[u8],
        _target: SocketAddr,
    ) -> Poll<io::Result<usize>> {
        todo!()
    }

    pub fn try_send_to(&self, _buf: &[u8], _target: SocketAddr) -> io::Result<usize> {
        todo!()
    }

    async fn send_to_addr(&self, _buf: &[u8], _target: SocketAddr) -> io::Result<usize> {
        todo!()
    }

    pub async fn recv_from(&self, _buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        todo!()
    }

    pub fn poll_recv_from(
        &self,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<SocketAddr>> {
        todo!()
    }

    pub fn try_recv_from(&self, _buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        todo!()
    }

    pub fn try_io<R>(
        &self,
        _interest: Interest,
        _f: impl FnOnce() -> io::Result<R>,
    ) -> io::Result<R> {
        todo!()
    }

    pub async fn peek_from(&self, _buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        todo!()
    }

    pub fn poll_peek_from(
        &self,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<SocketAddr>> {
        todo!()
    }

    pub fn broadcast(&self) -> io::Result<bool> {
        unimplemented!("not supported in simulator")
    }

    pub fn set_broadcast(&self, _on: bool) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn multicast_loop_v4(&self) -> io::Result<bool> {
        unimplemented!("not supported in simulator")
    }

    pub fn set_multicast_loop_v4(&self, _on: bool) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn multicast_ttl_v4(&self) -> io::Result<u32> {
        unimplemented!("not supported in simulator")
    }

    pub fn set_multicast_ttl_v4(&self, _ttl: u32) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn multicast_loop_v6(&self) -> io::Result<bool> {
        unimplemented!("not supported in simulator")
    }

    pub fn set_multicast_loop_v6(&self, _on: bool) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn ttl(&self) -> io::Result<u32> {
        unimplemented!("not supported in simulator")
    }

    pub fn set_ttl(&self, _ttl: u32) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn join_multicast_v4(&self, _multiaddr: Ipv4Addr, _interface: Ipv4Addr) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn join_multicast_v6(&self, _multiaddr: &Ipv6Addr, _interface: u32) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn leave_multicast_v4(&self, _multiaddr: Ipv4Addr, _interface: Ipv4Addr) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn leave_multicast_v6(&self, _multiaddr: &Ipv6Addr, _interface: u32) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        unimplemented!("not supported in simulator")
    }
}

impl TryFrom<std::net::UdpSocket> for UdpSocket {
    type Error = io::Error;

    fn try_from(stream: std::net::UdpSocket) -> Result<Self, Self::Error> {
        Self::from_std(stream)
    }
}
