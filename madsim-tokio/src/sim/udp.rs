use log::trace;

use std::{
    io, net,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use madsim::net::Endpoint;
use real_tokio::io::{Interest, ReadBuf, Ready};

use bytes::BufMut;

use crate::poller::Poller;

#[derive(Debug)]
pub struct UdpSocket {
    ep: Arc<Endpoint>,
    default_dest: Mutex<Option<SocketAddr>>,
}

impl UdpSocket {
    fn new_from_ep(ep: Arc<Endpoint>) -> Self {
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
        Ok(Self::new_from_ep(ep))
    }

    fn new(_socket: mio::net::UdpSocket) -> io::Result<UdpSocket> {
        unimplemented!("cannot create udp socket from mio::net::UdpSocket")
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

    pub async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        todo!()
    }

    pub async fn writable(&self) -> io::Result<()> {
        self.ready(Interest::WRITABLE).await?;
        Ok(())
    }

    pub fn poll_send_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        todo!()
    }

    pub fn poll_send(&self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        todo!()
    }

    pub fn try_send(&self, buf: &[u8]) -> io::Result<usize> {
        todo!()
    }

    pub async fn readable(&self) -> io::Result<()> {
        self.ready(Interest::READABLE).await?;
        Ok(())
    }

    pub fn poll_recv_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        todo!()
    }

    pub fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        todo!()
    }

    pub fn try_recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        todo!()
    }

    pub fn try_recv_buf<B: BufMut>(&self, buf: &mut B) -> io::Result<usize> {
        todo!()
    }

    pub fn try_recv_buf_from<B: BufMut>(&self, buf: &mut B) -> io::Result<(usize, SocketAddr)> {
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
        cx: &mut Context<'_>,
        buf: &[u8],
        target: SocketAddr,
    ) -> Poll<io::Result<usize>> {
        todo!()
    }

    pub fn try_send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        todo!()
    }

    async fn send_to_addr(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        todo!()
    }

    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        todo!()
    }

    pub fn poll_recv_from(
        &self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<SocketAddr>> {
        todo!()
    }

    pub fn try_recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        todo!()
    }

    pub fn try_io<R>(
        &self,
        interest: Interest,
        f: impl FnOnce() -> io::Result<R>,
    ) -> io::Result<R> {
        todo!()
    }

    pub async fn peek_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        todo!()
    }

    pub fn poll_peek_from(
        &self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<SocketAddr>> {
        todo!()
    }

    pub fn broadcast(&self) -> io::Result<bool> {
        unimplemented!("not supported in simulator")
    }

    pub fn set_broadcast(&self, on: bool) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn multicast_loop_v4(&self) -> io::Result<bool> {
        unimplemented!("not supported in simulator")
    }

    pub fn set_multicast_loop_v4(&self, on: bool) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn multicast_ttl_v4(&self) -> io::Result<u32> {
        unimplemented!("not supported in simulator")
    }

    pub fn set_multicast_ttl_v4(&self, ttl: u32) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn multicast_loop_v6(&self) -> io::Result<bool> {
        unimplemented!("not supported in simulator")
    }

    pub fn set_multicast_loop_v6(&self, on: bool) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn ttl(&self) -> io::Result<u32> {
        unimplemented!("not supported in simulator")
    }

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn join_multicast_v4(&self, multiaddr: Ipv4Addr, interface: Ipv4Addr) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn join_multicast_v6(&self, multiaddr: &Ipv6Addr, interface: u32) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn leave_multicast_v4(&self, multiaddr: Ipv4Addr, interface: Ipv4Addr) -> io::Result<()> {
        unimplemented!("not supported in simulator")
    }

    pub fn leave_multicast_v6(&self, multiaddr: &Ipv6Addr, interface: u32) -> io::Result<()> {
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
