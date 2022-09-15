use crate::{plugin, rand::*, task::NodeId, time::TimeHandle};
use futures::channel::oneshot;
use serde::{Deserialize, Serialize};
use std::{
    any::Any,
    collections::{hash_map::Entry, HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    io,
    net::{IpAddr, SocketAddr},
    ops::Range,
    sync::{Arc, Mutex},
    task::{Context, Waker},
    time::Duration,
};
use tracing::*;

/// A simulated network.
pub(crate) struct Network {
    rand: GlobalRng,
    time: TimeHandle,
    config: Config,
    stat: Stat,
    nodes: HashMap<NodeId, Node>,
    /// Maps the global IP to its node.
    addr_to_node: HashMap<IpAddr, NodeId>,
    clogged_node: HashSet<NodeId>,
    clogged_link: HashSet<(NodeId, NodeId)>,
}

/// Network for a node.
#[derive(Default)]
struct Node {
    /// IP address of the node.
    ///
    /// NOTE: now a node can have at most one IP address.
    ip: Option<IpAddr>,
    /// Sockets in the node.
    sockets: HashMap<u16, Arc<Mutex<Mailbox>>>,
}

/// Network configurations.
#[cfg_attr(docsrs, doc(cfg(msim)))]
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Config {
    /// Possibility of packet loss.
    #[serde(default)]
    pub packet_loss_rate: f64,
    /// The latency range of sending packets.
    #[serde(default = "default_send_latency")]
    pub send_latency: Range<Duration>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            packet_loss_rate: 0.0,
            send_latency: default_send_latency(),
        }
    }
}

const fn default_send_latency() -> Range<Duration> {
    Duration::from_millis(1)..Duration::from_millis(10)
}

#[allow(clippy::derive_hash_xor_eq)]
impl Hash for Config {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.packet_loss_rate.to_bits().hash(state);
        self.send_latency.hash(state);
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
    pub fn new(rand: GlobalRng, time: TimeHandle, config: Config) -> Self {
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

    pub fn update_config(&mut self, f: impl FnOnce(&mut Config)) {
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
            let port = (32768..=u16::MAX)
                .find(|port| !node.sockets.contains_key(port))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::AddrInUse, "no available ephemeral port")
                })?;
            addr.set_port(port);
        }
        // insert socket
        debug!("bound: {addr} -> {node_id}");
        match node.sockets.entry(addr.port()) {
            Entry::Occupied(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("address already in use: {addr}"),
                ))
            }
            Entry::Vacant(o) => {
                o.insert(Default::default());
            }
        }
        Ok(addr)
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

    pub fn close(&mut self, node: NodeId, addr: SocketAddr) {
        debug!("close: {node} {addr}");
        let node = self.nodes.get_mut(&node).expect("node not found");
        // TODO: simulate TIME_WAIT?
        node.sockets.remove(&addr.port());
    }

    pub fn send(
        &mut self,
        node: NodeId,
        src: SocketAddr,
        dst: SocketAddr,
        tag: u64,
        data: Payload,
    ) {
        trace!("send: {node} {src} -> {dst}, tag={tag:x}");
        let dst_node = if dst.ip().is_loopback() {
            node
        } else if let Some(x) = self.addr_to_node.get(&dst.ip()) {
            *x
        } else {
            trace!("destination not found: {dst}");
            return;
        };
        if self.clogged_node.contains(&node)
            || self.clogged_node.contains(&dst_node)
            || self.clogged_link.contains(&(node, dst_node))
        {
            trace!("clogged");
            return;
        }
        if self.rand.gen_bool(self.config.packet_loss_rate) {
            trace!("packet loss");
            return;
        }
        let ep = match self.nodes[&dst_node].sockets.get(&dst.port()) {
            Some(ep) => ep.clone(),
            None => {
                trace!("destination not found: {dst}");
                return;
            }
        };
        let msg = Message {
            tag,
            data,
            from: src,
        };
        let latency = self.rand.gen_range(self.config.send_latency.clone());
        trace!("delay: {latency:?}");
        self.time
            .add_timer(self.time.now_instant() + latency, move || {
                trace!("deliver: {node} {src} -> {dst}, tag={tag:x}");
                ep.lock().unwrap().deliver(msg);
            });
        self.stat.msg_count += 1;
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
        cx: &mut Context<'_>,
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

pub type Payload = Box<dyn Any + Send + Sync>;

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

    fn recv_ready(&mut self, cx: &mut Context<'_>, tag: u64) -> bool {
        let ready = self.msgs.iter().any(|msg| tag == msg.tag);
        if !ready {
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
