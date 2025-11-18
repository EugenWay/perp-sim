// latency.rs
// Latency models defining how long messages take to travel between agents
// and how much "compute time" the receiver needs.

use crate::messages::AgentId;

/// Latency model for simulated network + compute delays.
pub trait LatencyModel {
    /// Network delay for a message travelling from `from` to `to`.
    fn delay_ns(&self, from: AgentId, to: AgentId) -> u64;

    /// Optional compute time on the receiver side.
    fn compute_ns(&self, _agent_id: AgentId) -> u64 {
        0
    }
}

/// Very simple latency model: fixed network and compute delays for all messages.
pub struct FixedLatency {
    network_delay_ns: u64,
    compute_delay_ns: u64,
}

impl FixedLatency {
    /// Create a fixed-latency model.
    /// `network_delay_ns` - delay for any message.
    /// `compute_delay_ns` - extra compute delay.
    pub fn new(network_delay_ns: u64, compute_delay_ns: u64) -> Self {
        Self {
            network_delay_ns,
            compute_delay_ns,
        }
    }
}

impl LatencyModel for FixedLatency {
    fn delay_ns(&self, _from: AgentId, _to: AgentId) -> u64 {
        self.network_delay_ns
    }

    fn compute_ns(&self, _agent_id: AgentId) -> u64 {
        self.compute_delay_ns
    }
}
