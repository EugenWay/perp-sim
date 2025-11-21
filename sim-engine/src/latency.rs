use crate::messages::AgentId;

pub trait LatencyModel {
    fn delay_ns(&self, from: AgentId, to: AgentId) -> u64;
    
    fn compute_ns(&self, _agent_id: AgentId) -> u64 {
        0
    }
}

pub struct FixedLatency {
    network_delay_ns: u64,
    compute_delay_ns: u64,
}

impl FixedLatency {
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
