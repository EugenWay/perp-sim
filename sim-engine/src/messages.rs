pub type AgentId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    PositionLiquidated, // Notify trader their position was liquidated
    MarketState,        // Broadcast OI and liquidity data
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    #[serde(alias = "long", alias = "Long")]
    Buy,
    #[serde(alias = "short", alias = "Short")]
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
    pub qty: f64,
    pub price: u64,
}

#[derive(Debug, Clone)]
pub struct MarketOrderPayload {
    pub symbol: String,
    pub side: Side,
    pub qty: f64,      // Number of tokens as float (e.g., 0.5 = 0.5 ETH, 2.0 = 2 ETH)
    pub leverage: u32, // 1-100x, default 5x
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

/// Market state snapshot for trader logic (OI + liquidity in micro-USD)
#[derive(Debug, Clone)]
pub struct MarketStatePayload {
    pub symbol: String,
    pub oi_long_usd: i128,
    pub oi_short_usd: i128,
    pub liquidity_usd: i128,
}

#[derive(Debug, Clone)]
pub struct LiquidationTaskPayload {
    pub symbol: String,
    pub max_positions: u32,
}

/// Notification sent to trader when their position is liquidated
#[derive(Debug, Clone)]
pub struct PositionLiquidatedPayload {
    pub symbol: String,
    pub side: Side,
    pub size_usd: i128,
    pub pnl: i128, // Final PnL (negative = loss)
    pub collateral_lost: i128,
}

/// Notification sent to trader when their order is executed
#[derive(Debug, Clone)]
pub struct OrderExecutedPayload {
    pub symbol: String,
    pub side: Side,
    pub order_type: OrderExecutionType,
    pub collateral_delta: i128, // + locked, - returned
    pub pnl: i128,              // PnL on close (0 for open)
    pub size_usd: i128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderExecutionType {
    Increase, // Opening/increasing position
    Decrease, // Closing/decreasing position
    Liquidation,
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
    PositionLiquidated(PositionLiquidatedPayload),
    OrderExecuted(OrderExecutedPayload),
    MarketState(MarketStatePayload),
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

    /// Emit a high-level event to the event bus (for logging/analytics).
    fn emit_event(&mut self, event: crate::events::SimEvent);
}
