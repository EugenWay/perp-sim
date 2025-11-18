// src/agents/mod.rs
// Common Agent trait and agent modules.

use crate::messages::{AgentId, Message, SimulatorApi};

pub mod exchange_agent;
pub mod oracle_agent;
pub mod trader_agent;

/// Core trait for all agents in the simulation.
pub trait Agent {
    fn id(&self) -> AgentId;
    fn name(&self) -> &str;

    /// Called once at simulation start.
    fn on_start(&mut self, _sim: &mut dyn SimulatorApi) {}

    /// Called when simulation ends.
    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {}

    /// Called when a wakeup event reaches this agent.
    /// `now_ns` is the simulation time for this wakeup.
    fn on_wakeup(&mut self, _sim: &mut dyn SimulatorApi, _now_ns: u64) {}

    /// Called when a message is delivered to this agent.
    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, _msg: &Message) {}
}
