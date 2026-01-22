//! Network configuration

use rand::{
    distributions::{Distribution, Uniform},
    seq::SliceRandom,
};
use std::{collections::HashMap, ops::Range, sync::Arc, time::Duration};

use crate::{rand::*, task::NodeId};

/// Defines a latency distribution.
#[derive(Debug, PartialEq, Clone, Hash)]
pub enum LatencyDistribution {
    /// A constant, unvarying distribution.
    Constant(Duration),
    /// A uniform distribution.
    Uniform(Range<Duration>),
    /// A compound weighted distribution.
    Compound(Vec<(u32, Box<LatencyDistribution>)>),
}

impl LatencyDistribution {
    /// Construct a uniform distribution
    pub fn uniform(range: Range<Duration>) -> Self {
        Self::Uniform(range)
    }

    /// Construct a compound distribution which selects from a weighted set of distributions.
    pub fn compound(range: impl IntoIterator<Item = (f64, LatencyDistribution)>) -> Self {
        Self::Compound(
            range
                .into_iter()
                .map(|(w, d)| ((w * 10000.0) as u32, Box::new(d)))
                .collect(),
        )
    }

    /// Make a bimodal distribution
    pub fn bimodal(main: Range<Duration>, exceptional: Range<Duration>, freq: f64) -> Self {
        assert!(freq > 0.0 && freq < 1.0);

        Self::compound([
            (1.0 - freq, Self::uniform(main)),
            (freq, Self::uniform(exceptional)),
        ])
    }

    /// Sample latency
    pub fn sample<R: Rng>(&self, rng: &mut R) -> Duration {
        match self {
            Self::Constant(dur) => *dur,
            Self::Uniform(range) => {
                let u = Uniform::from(range.clone());
                u.sample(rng)
            }
            Self::Compound(sub_distributions) => {
                let sub_dist = &sub_distributions[..]
                    .choose_weighted(rng, |item| item.0)
                    .unwrap()
                    .1;
                sub_dist.sample(rng)
            }
        }
    }
}

/// Trait for defining latency between two nodes
pub trait InterNodeLatency {
    /// Get the latency between a and b
    fn sample(&self, rng: &mut GlobalRng, a: NodeId, b: NodeId) -> Option<Duration>;
}

impl std::fmt::Debug for dyn InterNodeLatency + Send + Sync + 'static {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<dyn InterNodeLatency>")
    }
}

/// Defines latency between pairs of nodes.
pub struct InterNodeLatencyMap(HashMap<(NodeId, NodeId), LatencyDistribution>);

impl InterNodeLatencyMap {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Add a latency distribution for both directions of a link.
    pub fn with_symmetric_link(mut self, a: NodeId, b: NodeId, dist: LatencyDistribution) -> Self {
        self.0.insert((a, b), dist.clone());
        self.0.insert((b, a), dist);
        self
    }
}

impl Default for InterNodeLatencyMap {
    fn default() -> Self {
        Self::new()
    }
}

impl InterNodeLatency for InterNodeLatencyMap {
    fn sample(&self, rng: &mut GlobalRng, a: NodeId, b: NodeId) -> Option<Duration> {
        // We try (a, b) first, then (b, a) - this way you can have asymetric latency if you want,
        // but you get symetric by default.
        if let Some(dist) = self.0.get(&(a, b)) {
            return Some(dist.sample(rng));
        }

        if let Some(dist) = self.0.get(&(b, a)) {
            return Some(dist.sample(rng));
        }

        return None;
    }
}

/// Defines latency from a given node to anywhere else. If a and b are
/// both in the map, both distributions are sampled and the max is taken.
pub struct NodeLatencyMap(HashMap<NodeId, LatencyDistribution>);

impl NodeLatencyMap {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Add a latency distribution for a specific node.
    pub fn with_node(mut self, node: NodeId, dist: LatencyDistribution) -> Self {
        self.0.insert(node, dist);
        self
    }
}

impl Default for NodeLatencyMap {
    fn default() -> Self {
        Self::new()
    }
}

impl InterNodeLatency for NodeLatencyMap {
    fn sample(&self, rng: &mut GlobalRng, a: NodeId, b: NodeId) -> Option<Duration> {
        match (self.0.get(&a), self.0.get(&b)) {
            (Some(a), Some(b)) => {
                let a = a.sample(rng);
                let b = b.sample(rng);
                Some(std::cmp::max(a, b))
            }
            (Some(a), None) => Some(a.sample(rng)),
            (None, Some(b)) => Some(b.sample(rng)),
            (None, None) => None,
        }
    }
}

/// Latency configuration
#[derive(Debug, Clone)]
pub struct LatencyConfig {
    /// The latency range of sending packets between nodes
    pub default_latency: LatencyDistribution,

    /// Latency for sending a packet to yourself.
    pub loopback_latency: LatencyDistribution,

    /// Latency distributions to use between two specified nodes
    /// When sending from A to B, we first look for (A, B) - if it is not found, we check for
    /// (B, A), which give us symmetrical latencies by default, with the option to specify
    /// asymetric latencies.
    pub inter_node_latency: Option<Arc<dyn InterNodeLatency + Send + Sync + 'static>>,
}

impl LatencyConfig {
    /// Get the latency between two nodes for a single packet.
    pub fn get_latency(&self, rng: &mut GlobalRng, a: NodeId, b: NodeId) -> Duration {
        if a == b {
            self.loopback_latency.sample(rng)
        } else {
            self.inter_node_latency
                .as_ref()
                .and_then(|lat| lat.sample(rng, a, b))
                .unwrap_or_else(|| self.default_latency.sample(rng))
        }
    }
}

impl Default for LatencyConfig {
    fn default() -> Self {
        Self {
            default_latency: LatencyDistribution::Uniform(
                Duration::from_millis(1)..Duration::from_millis(10),
            ),
            loopback_latency: LatencyDistribution::Uniform(
                Duration::from_micros(1)..Duration::from_micros(10),
            ),
            inter_node_latency: None,
        }
    }
}

/// Return packet loss probability between two nodes
pub trait NodePacketLoss {
    /// Return packet loss probability between two nodes, or return None to fall back to the
    /// default.
    fn packet_loss_rate(&self, rng: &mut GlobalRng, a: NodeId, b: NodeId) -> Option<f64>;
}

/// Defines probability of dropping a packet sent or received by a node.
/// If both sender and receiver appear in the map we use the higher probability.
pub struct NodePacketLossMap(HashMap<NodeId, f64>);

impl NodePacketLoss for NodePacketLossMap {
    fn packet_loss_rate(&self, _rng: &mut GlobalRng, a: NodeId, b: NodeId) -> Option<f64> {
        match (self.0.get(&a), self.0.get(&b)) {
            (Some(a), Some(b)) => Some(a.max(*b)),
            (Some(a), None) => Some(*a),
            (None, Some(b)) => Some(*b),
            (None, None) => None,
        }
    }
}

impl std::fmt::Debug for dyn NodePacketLoss + Send + Sync + 'static {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<dyn NodePacketLoss>")
    }
}

/// Packet Loss configuration
#[derive(Debug, Clone, Default)]
pub struct PacketLossConfig {
    /// Default loopback packet loss probability
    pub loopback_packet_loss_rate: f64,

    /// Default network packet loss probability
    pub default_packet_loss_rate: f64,

    /// Packet packet loss probability between two nodes
    pub node_packet_loss_rate: Option<Arc<dyn NodePacketLoss + Sync + Send + 'static>>,
}

impl PacketLossConfig {
    /// Get the packet loss rate between two nodes
    pub fn packet_loss_rate(&self, rng: &mut GlobalRng, a: NodeId, b: NodeId) -> f64 {
        if a == b {
            return self.loopback_packet_loss_rate;
        }

        self.node_packet_loss_rate
            .as_ref()
            .map(|npl| npl.packet_loss_rate(rng, a, b))
            .unwrap_or(Some(self.default_packet_loss_rate))
            .unwrap()
    }
}

/// Network configurations.
#[cfg_attr(docsrs, doc(cfg(msim)))]
#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    /// Possibility of packet loss (for UDP connections only).
    pub packet_loss: PacketLossConfig,

    /// Packet loss for tcp causes the connection to be reset, hence has a different config.
    pub tcp_packet_loss: PacketLossConfig,

    /// Latency configuraion
    pub latency: LatencyConfig,
}
