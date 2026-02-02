use crate::messages::{AgentId, MessageType, Side};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type")]
pub enum SimEvent {
    /// Order submitted (before execution)
    OrderLog {
        ts: u64,
        from: AgentId,
        to: AgentId,
        msg_type: MessageType,
        symbol: Option<String>,
        side: Option<Side>,
        price: Option<u64>,
        qty: Option<f64>,
    },

    /// Order executed by exchange
    OrderExecuted {
        ts: u64,
        account: AgentId,
        symbol: String,
        side: Side,
        size_usd: u64,       // Position size in micro-USD
        collateral: u64,     // Collateral in micro-USD
        execution_price: u64, // Execution price in micro-USD
        leverage: u32,
        order_type: String,  // "Increase", "Decrease", "Liquidation"
        pnl: i64,            // PnL on close (0 for open)
    },

    /// Oracle price update
    OracleTick {
        ts: u64,
        symbol: String,
        price_min: u64,
        price_max: u64,
    },

    /// Position snapshot (periodic state dump for analysis)
    PositionSnapshot {
        ts: u64,
        account: AgentId,
        symbol: String,
        side: Side,
        size_usd: u64,
        size_tokens: i128,
        collateral: u64,
        entry_price: u64,       // Calculated: size_usd / size_tokens
        current_price: u64,     // From oracle
        unrealized_pnl: i64,    // Calculated on our side (not from engine)
        liquidation_price: u64, // TODO(perp-futures): get from engine
        leverage_actual: u32,   // size_usd / collateral
        is_liquidatable: bool,  // current_price crossed liquidation_price
        opened_at_sec: u64,
    },

    /// Market state snapshot
    MarketSnapshot {
        ts: u64,
        symbol: String,
        oi_long_usd: u64,
        oi_short_usd: u64,
        liquidity_usd: u64,
        /// Funding rate per hour in bps * 1e6 (signed; + = longs pay, - = shorts pay)
        funding_rate_bps_hour_fp: i64,
        /// Borrowing rate per hour in bps * 1e6 (unsigned)
        borrowing_rate_bps_hour_fp: u64,
    },

    /// Position was liquidated (explicit event for frontend/analytics)
    PositionLiquidated {
        ts: u64,
        account: AgentId,
        symbol: String,
        side: Side,
        size_usd: u64,
        collateral_lost: u64,  // Collateral that was seized/lost
        pnl: i64,              // Final PnL (usually negative)
        liquidation_price: u64, // Price at which liquidated
    },
}

pub trait EventListener {
    fn on_event(&mut self, event: &SimEvent);
}

pub struct EventBus {
    listeners: Vec<Box<dyn EventListener>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self { listeners: Vec::new() }
    }

    /// Subscribe a new listener.
    pub fn subscribe(&mut self, listener: Box<dyn EventListener>) {
        self.listeners.push(listener);
    }

    /// Emit an event to all listeners.
    pub fn emit(&mut self, event: SimEvent) {
        for listener in self.listeners.iter_mut() {
            listener.on_event(&event);
        }
    }
}
