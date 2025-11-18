// src/messages.rs
// Core message types and simulator API used by all agents.

/// Numeric identifier of an agent in the simulation.
pub type AgentId = u32;

/// High level message type.
/// You can extend this enum as the protocol evolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageType {
    Wakeup,

    // Trading commands
    LimitOrder,
    MarketOrder,
    CancelOrder,
    ModifyOrder,

    // Queries
    QuerySpread,
    QueryLast,

    // Market data / events
    MarketData,
    Trade,
    OrderLog, // generic log event

    // Oracle
    OracleTick,

    // Exchange responses
    OrderAccepted,
    OrderExecuted,
    OrderCancelled,
    OrderRejected,

    // Risk / liquidations
    LiquidationScan,
    LiquidationExecute,
}

/// Basic side enum for orders and trades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// Simple payload types.
/// For the first iteration we keep payloads minimal.
#[derive(Debug, Clone)]
pub struct LimitOrderPayload {
    pub symbol: String,
    pub side: Side,
    pub qty: u64,
    pub price: u64,
}

#[derive(Debug, Clone)]
pub struct MarketOrderPayload {
    pub symbol: String,
    pub side: Side,
    pub qty: u64,
}

/// Oracle tick in simulation.
/// Right now it is just (symbol, price); later you can extend it
/// to min/max like your on-chain Price.
#[derive(Debug, Clone)]
pub struct OracleTickPayload {
    pub symbol: String,
    pub price: u64,
}

#[derive(Debug, Clone)]
pub struct LiquidationTaskPayload {
    pub symbol: String,
    pub max_positions: u32,
}

/// Generic payload enum.
#[derive(Debug, Clone)]
pub enum MessagePayload {
    Empty,
    Text(String),
    LimitOrder(LimitOrderPayload),
    MarketOrder(MarketOrderPayload),
    OracleTick(OracleTickPayload),
    LiquidationTask(LiquidationTaskPayload),
}

/// Core message type that flows through the Kernel.
#[derive(Debug, Clone)]
pub struct Message {
    pub to: AgentId,
    pub from: AgentId,
    pub msg_type: MessageType,
    /// Simulation time in nanoseconds when this message should be delivered.
    pub at: u64,
    pub payload: MessagePayload,
}

impl Message {
    pub fn new(to: AgentId, from: AgentId, msg_type: MessageType, at: u64, payload: MessagePayload) -> Self {
        Self {
            to,
            from,
            msg_type,
            at,
            payload,
        }
    }

    /// Helper constructor for a message with empty payload.
    pub fn new_empty(to: AgentId, from: AgentId, msg_type: MessageType, at: u64) -> Self {
        Self {
            to,
            from,
            msg_type,
            at,
            payload: MessagePayload::Empty,
        }
    }
}

/// Minimal interface that the kernel exposes to agents.
pub trait SimulatorApi {
    /// Return current simulation time in nanoseconds.
    fn now_ns(&self) -> u64;

    /// Send a message from one agent to another.
    fn send(&mut self, from: AgentId, to: AgentId, kind: MessageType, payload: MessagePayload);

    /// Schedule a wakeup for a specific agent at the given simulation time.
    fn wakeup(&mut self, agent_id: AgentId, at_ns: u64);

    /// Broadcast a message from one agent to all others.
    fn broadcast(&mut self, from: AgentId, kind: MessageType, payload: MessagePayload);
}
