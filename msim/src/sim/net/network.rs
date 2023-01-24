use super::config::NetworkConfig;
use crate::{plugin, rand::*, task::NodeId, time::TimeHandle};
use futures::channel::oneshot;
use std::{
    any::Any,
    collections::{hash_map::Entry, HashMap, HashSet, VecDeque},
    io,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    task::{Context, Waker},
};

use tap::TapOptional;
use tracing::*;

/// A simulated network.
pub(crate) struct Network {
    rand: GlobalRng,
    time: TimeHandle,
    config: NetworkConfig,
    stat: Stat,
    nodes: HashMap<NodeId, Node>,
    /// Maps the global IP to its node.
    addr_to_node: HashMap<IpAddr, NodeId>,
    clogged_node: HashSet<NodeId>,
    clogged_link: HashSet<(NodeId, NodeId)>,
}

/// Network for a node.
struct Node {
    /// IP address of the node.
    ///
    /// NOTE: now a node can have at most one IP address.
    ip: Option<IpAddr>,
    /// Sockets in the node.
    sockets: HashMap<u16, Arc<Mutex<Mailbox>>>,

    /// live tcp connections.
    live_tcp_ids: HashSet<u32>,

    /// Next ephemeral port. There is some code in sui/narwhal that wants to pick ports in advance
    /// and then bind to them later. This is done in narwhal by binding to an ephemeral port,
    /// making a connection, and then relying on time-wait behavior to reserve the port until it is
    /// used later.
    ///
    /// Instead of simulating time-wait behavior we just don't hand out the same port twice if we
    /// can help it.
    next_ephemeral_port: u16,
}

impl Default for Node {
    fn default() -> Self {
        Self {
            ip: None,
            sockets: HashMap::new(),
            live_tcp_ids: HashSet::new(),
            next_ephemeral_port: 0x8000,
        }
    }
}

/// Network statistics.
#[cfg_attr(docsrs, doc(cfg(msim)))]
#[derive(Debug, Default, Clone)]
pub struct Stat {
    /// Total number of messages.
    pub msg_count: u64,
}

impl Network {
    pub fn new(rand: GlobalRng, time: TimeHandle, config: NetworkConfig) -> Self {
        Self {
            rand,
            time,
            config,
            stat: Stat::default(),
            nodes: HashMap::new(),
            addr_to_node: HashMap::new(),
            clogged_node: HashSet::new(),
            clogged_link: HashSet::new(),
        }
    }

    fn get_node_for_addr(&self, ip: &IpAddr) -> Option<NodeId> {
        if ip.is_loopback() {
            Some(plugin::node())
        } else {
            self.addr_to_node.get(ip).cloned()
        }
    }

    pub fn update_config(&mut self, f: impl FnOnce(&mut NetworkConfig)) {
        f(&mut self.config);
    }

    pub fn stat(&self) -> &Stat {
        &self.stat
    }

    pub fn insert_node(&mut self, id: NodeId) {
        debug!("insert: {id}");
        self.nodes.insert(id, Default::default());
    }

    pub fn reset_node(&mut self, id: NodeId) {
        debug!("reset: {id}");
        let node = self.nodes.get_mut(&id).expect("node not found");
        // close all sockets
        node.sockets.clear();
    }

    pub fn delete_node(&mut self, id: NodeId) {
        debug!("delete: {id}");
        let node = self.nodes.remove(&id).expect("node not found");

        if let Some(ip) = &node.ip {
            self.addr_to_node.remove(ip);
        }
        self.clogged_node.remove(&id);

        let to_remove: Vec<_> = self
            .clogged_link
            .iter()
            .filter_map(|(a, b)| {
                if *a == id || *b == id {
                    Some((*a, *b))
                } else {
                    None
                }
            })
            .collect();

        for k in &to_remove {
            self.clogged_link.remove(k);
        }
    }

    pub fn set_ip(&mut self, id: NodeId, ip: IpAddr) {
        debug!("set-ip: {id}: {ip}");
        let node = self.nodes.get_mut(&id).expect("node not found");
        if let Some(old_ip) = node.ip.replace(ip) {
            self.addr_to_node.remove(&old_ip);
        }
        let old_node = self.addr_to_node.insert(ip, id);
        if let Some(old_node) = old_node {
            panic!("IP conflict: {ip} {old_node}");
        }
        // TODO: what if we change the IP when there are opening sockets?
    }

    pub fn get_ip(&self, id: NodeId) -> Option<IpAddr> {
        let node = self.nodes.get(&id).expect("node not found");
        node.ip
    }

    pub fn clog_node(&mut self, id: NodeId) {
        assert!(self.nodes.contains_key(&id));
        debug!("clog: {id}");
        self.clogged_node.insert(id);
    }

    pub fn unclog_node(&mut self, id: NodeId) {
        assert!(self.nodes.contains_key(&id));
        debug!("unclog: {id}");
        self.clogged_node.remove(&id);
    }

    pub fn clog_link(&mut self, src: NodeId, dst: NodeId) {
        assert!(self.nodes.contains_key(&src));
        assert!(self.nodes.contains_key(&dst));
        debug!("clog: {src} -> {dst}");
        self.clogged_link.insert((src, dst));
    }

    pub fn unclog_link(&mut self, src: NodeId, dst: NodeId) {
        assert!(self.nodes.contains_key(&src));
        assert!(self.nodes.contains_key(&dst));
        debug!("unclog: {src} -> {dst}");
        self.clogged_link.remove(&(src, dst));
    }

    pub fn bind(&mut self, node_id: NodeId, mut addr: SocketAddr) -> io::Result<SocketAddr> {
        debug!("binding: {addr} -> {node_id}");
        let node = self.nodes.get_mut(&node_id).expect("node not found");
        // resolve IP if unspecified
        if addr.ip().is_unspecified() {
            if let Some(ip) = node.ip {
                addr.set_ip(ip);
            } else {
                todo!("try to bind 0.0.0.0, but the node IP is also unspecified");
            }
        } else if addr.ip().is_loopback() {
        } else if addr.ip() != node.ip.expect("node IP is unset") {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                format!("invalid address: {addr}"),
            ));
        }
        // resolve port if unspecified
        if addr.port() == 0 {
            let next_ephemeral_port = node.next_ephemeral_port;
            let port = (next_ephemeral_port..=u16::MAX)
                .find(|port| !node.sockets.contains_key(port))
                .ok_or_else(|| {
                    warn!("ephemeral ports exhausted");
                    io::Error::new(io::ErrorKind::AddrInUse, "no available ephemeral port")
                })?;
            node.next_ephemeral_port = port.wrapping_add(1);
            trace!("assigned ephemeral port {}", port);
            addr.set_port(port);
        }
        // insert socket
        match node.sockets.entry(addr.port()) {
            Entry::Occupied(_) => {
                warn!("bind() error: address already in use: {addr:?}");
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("address already in use: {addr}"),
                ));
            }
            Entry::Vacant(o) => {
                o.insert(Default::default());
            }
        }
        debug!("bound: {addr} -> {node_id}");
        Ok(addr)
    }

    pub fn register_tcp_id(&mut self, node_id: NodeId, tcp_id: u32) {
        trace!("registering tcp id {} for node {}", tcp_id, node_id);
        assert!(
            self.nodes
                .get_mut(&node_id)
                .unwrap()
                .live_tcp_ids
                .insert(tcp_id),
            "duplicate tcp id {}",
            tcp_id
        );
    }

    pub fn deregister_tcp_id(&mut self, node: NodeId, remote_addr: &SocketAddr, tcp_id: u32) {
        trace!("deregistering tcp id {} for node {}", tcp_id, node);

        // node may have been deleted
        if let Some(node) = self.nodes.get_mut(&node) {
            // remove id from node
            assert!(
                node.live_tcp_ids.remove(&tcp_id),
                "unknown tcp id {}",
                tcp_id
            );
        };

        // wake the remote end in case it is waiting on a read.
        let Some(node_id) = &self.get_node_for_addr(&remote_addr.ip()) else {
            // node may have been deleted
            debug!("No node found for {remote_addr}");
            return;
        };

        if let Some(socket) = self
            .nodes
            .get_mut(node_id)
            .map(|node| node.sockets.get(&remote_addr.port()))
            .tap_none(|| debug!("No node found for {node_id}"))
            .flatten()
        {
            socket.lock().unwrap().wake_tcp_connection(tcp_id);
        }
    }

    pub fn is_tcp_session_live(&self, peer: &SocketAddr, tcp_id: u32) -> bool {
        if let Some(node_id) = self.get_node_for_addr(&peer.ip()) {
            self.nodes[&node_id].live_tcp_ids.contains(&tcp_id)
        } else {
            // the node does not exist, it may have been killed / restarted.
            debug!("could not find node for id: {tcp_id}");
            false
        }
    }

    pub fn signal_connect(&self, src: SocketAddr, dst: SocketAddr) -> bool {
        let node = self.get_node_for_addr(&dst.ip());
        if node.is_none() {
            return false;
        }
        let node = node.unwrap();

        let dst_socket = self.nodes[&node].sockets.get(&dst.port());

        if let Some(dst_socket) = dst_socket {
            dst_socket.lock().unwrap().signal_connect(src);
            true
        } else {
            false
        }
    }

    pub fn accept_connect(&self, node: NodeId, listening: SocketAddr) -> Option<SocketAddr> {
        let socket = self.nodes[&node].sockets.get(&listening.port()).unwrap();
        socket.lock().unwrap().accept_connect()
    }

    pub fn close(&mut self, node_id: NodeId, addr: SocketAddr) {
        if let Some(node) = self.nodes.get_mut(&node_id) {
            debug!("close: {node_id} {addr}");
            // TODO: simulate TIME_WAIT?
            node.sockets.remove(&addr.port());
        }
    }

    pub fn send(
        &mut self,
        node_id: NodeId,
        src: SocketAddr,
        dst: SocketAddr,
        tag: u64,
        data: Payload,
    ) -> io::Result<()> {
        trace!("send: {node_id} {src} -> {dst}, tag={tag:x}");
        let dst_node = if dst.ip().is_loopback() {
            node_id
        } else if let Some(x) = self.addr_to_node.get(&dst.ip()) {
            *x
        } else {
            trace!("destination not found: {dst}");
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("host unreachable: {dst}"),
            ));
        };
        if self.clogged_node.contains(&node_id)
            || self.clogged_node.contains(&dst_node)
            || self.clogged_link.contains(&(node_id, dst_node))
        {
            trace!("clogged");
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("host unreachable: {dst}"),
            ));
        }

        match data.ty {
            PayloadType::Udp => {
                let plr =
                    self.config
                        .packet_loss
                        .packet_loss_rate(&mut self.rand, node_id, dst_node);
                if self.rand.gen_bool(plr) {
                    trace!("packet loss");
                    return Ok(());
                }
            }
            PayloadType::TcpSignalConnect | PayloadType::TcpData => {
                let plr =
                    self.config
                        .tcp_packet_loss
                        .packet_loss_rate(&mut self.rand, node_id, dst_node);
                if self.rand.gen_bool(plr) {
                    debug!("tcp connection failure");
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        format!("peer hung up: {dst}"),
                    ));
                }
            }
        }

        let node = &self.nodes[&dst_node];

        if data.is_tcp_data() {
            let id: u32 = (tag >> 32).try_into().unwrap();
            // sender learns instantaneously that the other end has hung up - this isn't very
            // realistic but it shouldn't matter for the most part. At some point we may build a
            // more physically-based tcp simulator.
            if !node.live_tcp_ids.contains(&id) {
                debug!("tcp session to {dst} has ended");
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    format!("peer hung up: {dst}"),
                ));
            }
        }

        let mailbox = match node.sockets.get(&dst.port()) {
            Some(mailbox) => Arc::downgrade(mailbox),
            None => {
                debug!("destination port not available: {dst}");
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("connection refused: {dst}"),
                ));
            }
        };

        let msg = Message {
            tag,
            data,
            from: src,
        };
        let latency = self
            .config
            .latency
            .get_latency(&mut self.rand, node_id, dst_node);
        trace!("delay: {latency:?}");
        self.time
            .add_timer_for_node(dst_node, self.time.now_instant() + latency, move || {
                if let Some(mailbox) = mailbox.upgrade() {
                    trace!(
                        "deliver: {src}(node: {node_id}) -> {dst}(node: {dst_node}), tag={tag:x}"
                    );
                    mailbox.lock().unwrap().deliver(msg);
                } else {
                    trace!("deliver: mailbox was destroyed before delivery");
                }
            });
        self.stat.msg_count += 1;

        Ok(())
    }

    pub fn recv(&mut self, node: NodeId, dst: SocketAddr, tag: u64) -> oneshot::Receiver<Message> {
        self.nodes[&node].sockets[&dst.port()]
            .lock()
            .unwrap()
            .recv(tag)
    }

    pub fn recv_sync(&mut self, node: NodeId, dst: SocketAddr, tag: u64) -> Option<Message> {
        self.nodes[&node].sockets[&dst.port()]
            .lock()
            .unwrap()
            .recv_sync(tag)
    }

    pub fn recv_ready(
        &self,
        cx: Option<&mut Context<'_>>,
        node: NodeId,
        dst: SocketAddr,
        tag: u64,
    ) -> bool {
        self.nodes[&node].sockets[&dst.port()]
            .lock()
            .unwrap()
            .recv_ready(cx, tag)
    }
}

pub struct Message {
    pub tag: u64,
    pub data: Payload,
    pub from: SocketAddr,
}

#[derive(Debug)]
pub enum PayloadType {
    TcpSignalConnect,
    TcpData,
    Udp,
}

pub struct Payload {
    pub ty: PayloadType,
    pub data: Box<dyn Any + Send + Sync>,
}

impl Payload {
    pub fn new_udp(data: Box<dyn Any + Send + Sync>) -> Self {
        Self {
            ty: PayloadType::Udp,
            data,
        }
    }

    pub fn new_tcp_connect(data: Box<dyn Any + Send + Sync>) -> Self {
        Self {
            ty: PayloadType::TcpSignalConnect,
            data,
        }
    }

    pub fn new_tcp_data(data: Box<dyn Any + Send + Sync>) -> Self {
        Self {
            ty: PayloadType::TcpData,
            data,
        }
    }

    pub fn is_udp(&self) -> bool {
        match self.ty {
            PayloadType::Udp => true,
            _ => false,
        }
    }

    pub fn is_tcp_data(&self) -> bool {
        match self.ty {
            PayloadType::TcpData => true,
            _ => false,
        }
    }

    pub fn is_tcp_connect(&self) -> bool {
        match self.ty {
            PayloadType::TcpSignalConnect => true,
            _ => false,
        }
    }
}

/// Tag message mailbox for an endpoint.
#[derive(Default)]
struct Mailbox {
    /// Pending receive requests.
    registered: Vec<(u64, oneshot::Sender<Message>)>,
    /// Messages that have not been received.
    msgs: Vec<Message>,

    /// Wakers for async io waiting for packets.
    wakers: Vec<(u64, Waker)>,

    /// tcp connections (via connect/accept) are signaled synchronously, out of band from the
    /// normal network simulation, in order to support blocking connect/accept.
    sync_connections: VecDeque<SocketAddr>,
}

impl Mailbox {
    fn wake_tcp_connection(&mut self, tcp_id: u32) {
        for i in (0..self.wakers.len()).rev() {
            if (self.wakers[i].0 >> 32) == tcp_id as u64 {
                let (_, waker) = self.wakers.swap_remove(i);
                waker.wake();
            }
        }
    }

    fn deliver(&mut self, msg: Message) {
        for i in (0..self.wakers.len()).rev() {
            if self.wakers[i].0 == msg.tag {
                let (_, waker) = self.wakers.swap_remove(i);
                waker.wake();
            }
        }

        let mut i = 0;
        let mut msg = Some(msg);
        while i < self.registered.len() {
            if matches!(&msg, Some(msg) if msg.tag == self.registered[i].0) {
                // tag match, take and try send
                let (_, sender) = self.registered.swap_remove(i);
                msg = match sender.send(msg.take().unwrap()) {
                    Ok(_) => return,
                    Err(m) => Some(m),
                };
                // failed to send, try next
            } else {
                // tag mismatch, move to next
                i += 1;
            }
        }
        // failed to match awaiting recv, save
        self.msgs.push(msg.unwrap());
    }

    fn recv_ready(&mut self, cx: Option<&mut Context<'_>>, tag: u64) -> bool {
        let ready = self.msgs.iter().any(|msg| tag == msg.tag);
        if let (false, Some(cx)) = (ready, cx) {
            self.wakers.push((tag, cx.waker().clone()));
        }
        ready
    }

    fn recv_sync(&mut self, tag: u64) -> Option<Message> {
        if let Some(idx) = self.msgs.iter().position(|msg| tag == msg.tag) {
            let msg = self.msgs.swap_remove(idx);
            Some(msg)
        } else {
            None
        }
    }

    fn recv(&mut self, tag: u64) -> oneshot::Receiver<Message> {
        let (tx, rx) = oneshot::channel();
        if let Some(msg) = self.recv_sync(tag) {
            tx.send(msg).ok().unwrap();
        } else {
            self.registered.push((tag, tx));
        }
        rx
    }

    fn signal_connect(&mut self, src_addr: SocketAddr) {
        self.sync_connections.push_back(src_addr);
    }

    fn accept_connect(&mut self) -> Option<SocketAddr> {
        self.sync_connections.pop_front()
    }
}
