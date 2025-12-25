pub type AgentId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageType {
    Wakeup,
    LimitOrder,
    MarketOrder,
    CloseOrder,
    CancelOrder,
    ModifyOrder,
    QuerySpread,
    QueryLast,
    MarketData,
    Trade,
    OrderLog,
    OracleTick,
    OrderAccepted,
    OrderExecuted,
    OrderCancelled,
    OrderRejected,
    LiquidationScan,
    LiquidationExecute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// Price range (bid/ask spread) for perpetual DEX
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price {
    pub min: u64, // lower bound (bid)
    pub max: u64, // upper bound (ask)
}

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

/// Close (decrease) an existing position
#[derive(Debug, Clone)]
pub struct CloseOrderPayload {
    pub symbol: String,
    pub side: Side, // Which side position to close (Buy=Long, Sell=Short)
}

/// Oracle price update with signature for on-chain verification.
/// Includes min/max range computed from confidence interval.
#[derive(Debug, Clone)]
pub struct OracleTickPayload {
    pub symbol: String,
    pub price: Price,       // min/max range (bid/ask)
    pub publish_time: u64,  // Unix timestamp (seconds)
    pub signature: Vec<u8>, // VAA signature from oracle provider (e.g., Pyth Network)
}

#[derive(Debug, Clone)]
pub struct LiquidationTaskPayload {
    pub symbol: String,
    pub max_positions: u32,
}

#[derive(Debug, Clone)]
pub enum MessagePayload {
    Empty,
    Text(String),
    LimitOrder(LimitOrderPayload),
    MarketOrder(MarketOrderPayload),
    CloseOrder(CloseOrderPayload),
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
