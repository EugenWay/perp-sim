//! Market Maker Agent
//!
//! Maintains OI balance by:
//! 1. Opening positions on the smaller side when imbalance exceeds threshold
//! 2. Closing positions when balance is restored
//! 3. Acting as "seed liquidity" to prevent death spirals
//!
//! This is NOT a traditional MM with bid/ask spread, but a "balancer" agent
//! that ensures healthy OI distribution for the simulation.

use crate::agents::Agent;
use crate::messages::{
    AgentId, MarketOrderPayload, MarketStatePayload, Message, MessagePayload, MessageType, OracleTickPayload,
    OrderExecutedPayload, OrderExecutionType, PositionLiquidatedPayload, Side, SimulatorApi,
};

/// Configuration for Market Maker
#[derive(Debug, Clone)]
pub struct MarketMakerConfig {
    pub name: String,
    pub exchange_id: AgentId,
    pub symbol: String,
    /// Target OI in micro-USD (e.g., 100_000_000_000 = $100k per side)
    pub target_oi_per_side: i128,
    /// Maximum imbalance percentage before acting (e.g., 20.0 = 20%)
    pub max_imbalance_pct: f64,
    /// Size of each MM order in tokens
    pub order_size_tokens: f64,
    /// Leverage to use (low for safety, e.g., 2x)
    pub leverage: u32,
    /// How often to check and rebalance (ms)
    pub wake_interval_ms: u64,
    /// Initial balance in micro-USD
    pub balance: i128,
}

impl Default for MarketMakerConfig {
    fn default() -> Self {
        Self {
            name: "MarketMaker".to_string(),
            exchange_id: 1,
            symbol: "ETH-USD".to_string(),
            target_oi_per_side: 150_000_000_000, // $150k per side target
            max_imbalance_pct: 30.0,             // Act when imbalance > 30%
            order_size_tokens: 2.0,              // 2 ETH per order
            leverage: 2,                         // Conservative 2x
            wake_interval_ms: 500,               // Check every 500ms
            balance: 1_000_000_000_000,          // $1M capital
        }
    }
}

pub struct MarketMakerAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    symbol: String,

    target_oi_per_side: i128,
    max_imbalance_pct: f64,
    order_size_tokens: f64,
    leverage: u32,
    wake_interval_ns: u64,

    // State tracking
    balance: i128,
    collateral_locked: i128,
    current_price: Option<u64>,

    // OI tracking
    oi_long_usd: i128,
    oi_short_usd: i128,

    // Position tracking (MM can have positions on both sides)
    long_position_size: i128,  // in micro-USD
    short_position_size: i128, // in micro-USD

    // Stats
    orders_placed: u32,
    rebalance_actions: u32,
}

impl MarketMakerAgent {
    pub fn new(id: AgentId, config: MarketMakerConfig) -> Self {
        Self {
            id,
            name: config.name,
            exchange_id: config.exchange_id,
            symbol: config.symbol,
            target_oi_per_side: config.target_oi_per_side,
            max_imbalance_pct: config.max_imbalance_pct,
            order_size_tokens: config.order_size_tokens,
            leverage: config.leverage,
            wake_interval_ns: config.wake_interval_ms * 1_000_000,
            balance: config.balance,
            collateral_locked: 0,
            current_price: None,
            oi_long_usd: 0,
            oi_short_usd: 0,
            long_position_size: 0,
            short_position_size: 0,
            orders_placed: 0,
            rebalance_actions: 0,
        }
    }

    /// Calculate OI imbalance percentage
    /// Positive = long-heavy, Negative = short-heavy
    fn calculate_imbalance_pct(&self) -> f64 {
        let total = self.oi_long_usd + self.oi_short_usd;
        if total == 0 {
            return 0.0;
        }
        (self.oi_long_usd - self.oi_short_usd) as f64 / total as f64 * 100.0
    }

    /// Check if market needs rebalancing
    fn needs_rebalancing(&self) -> Option<Side> {
        let imbalance = self.calculate_imbalance_pct();
        let total_oi = self.oi_long_usd + self.oi_short_usd;

        // Don't act if OI is too small (market still warming up)
        // But DO act if one side is completely empty
        if total_oi > 0 && (self.oi_long_usd == 0 || self.oi_short_usd == 0) {
            // Emergency: one side is empty!
            if self.oi_long_usd == 0 {
                return Some(Side::Buy);
            } else {
                return Some(Side::Sell);
            }
        }

        // Normal imbalance check
        if imbalance > self.max_imbalance_pct {
            // Long-heavy → open SHORT to balance
            Some(Side::Sell)
        } else if imbalance < -self.max_imbalance_pct {
            // Short-heavy → open LONG to balance
            Some(Side::Buy)
        } else {
            None
        }
    }

    /// Check if we need to provide seed liquidity
    fn needs_seed_liquidity(&self) -> Option<Side> {
        let total_oi = self.oi_long_usd + self.oi_short_usd;

        // If total OI is below threshold and price is available, seed both sides
        if total_oi < self.target_oi_per_side / 2 && self.current_price.is_some() {
            // Prioritize the smaller side
            if self.oi_long_usd <= self.oi_short_usd {
                return Some(Side::Buy);
            } else {
                return Some(Side::Sell);
            }
        }
        None
    }

    fn open_position(&mut self, sim: &mut dyn SimulatorApi, side: Side, reason: &str) {
        let price = match self.current_price {
            Some(p) => p as f64,
            None => return,
        };

        let size_usd = (self.order_size_tokens * price) as i128;
        let collateral_needed = size_usd / self.leverage as i128;

        // Check if we have enough balance
        let available = self.balance - self.collateral_locked;
        if collateral_needed > available {
            println!(
                "[MM {}] Insufficient balance for {} order: need ${:.2}, have ${:.2}",
                self.name,
                if side == Side::Buy { "LONG" } else { "SHORT" },
                collateral_needed as f64 / 1_000_000.0,
                available as f64 / 1_000_000.0
            );
            return;
        }

        let payload = MessagePayload::MarketOrder(MarketOrderPayload {
            symbol: self.symbol.clone(),
            side,
            qty: self.order_size_tokens,
            leverage: self.leverage,
        });

        println!(
            "[MM {}] {} {} {}x qty={:.3} @ ${:.2} (imbalance={:.1}%)",
            self.name,
            reason,
            if side == Side::Buy { "LONG" } else { "SHORT" },
            self.leverage,
            self.order_size_tokens,
            price / 1_000_000.0,
            self.calculate_imbalance_pct()
        );

        sim.send(self.id, self.exchange_id, MessageType::MarketOrder, payload);

        self.collateral_locked += collateral_needed;
        self.orders_placed += 1;

        match side {
            Side::Buy => self.long_position_size += size_usd,
            Side::Sell => self.short_position_size += size_usd,
        }
    }

    fn handle_order_executed(&mut self, payload: &OrderExecutedPayload) {
        match payload.order_type {
            OrderExecutionType::Increase => {
                // Position opened - actual collateral may differ
                let actual_collateral = payload.collateral_delta;
                // Adjust our tracking
                self.collateral_locked = (self.collateral_locked - actual_collateral).max(0) + actual_collateral;
            }
            OrderExecutionType::Decrease => {
                // Position closed - return collateral + PnL
                self.collateral_locked = (self.collateral_locked + payload.collateral_delta).max(0);
                self.balance += payload.pnl;

                match payload.side {
                    Side::Buy => self.long_position_size = 0,
                    Side::Sell => self.short_position_size = 0,
                }
            }
            OrderExecutionType::Liquidation => {
                // Oops - we got liquidated (shouldn't happen with 2x leverage normally)
                println!("[MM {}] WARNING: Got liquidated!", self.name);
            }
        }
    }

    fn handle_liquidation(&mut self, payload: &PositionLiquidatedPayload) {
        println!(
            "[MM {}] LIQUIDATED {} - lost ${:.2}",
            self.name,
            if payload.side == Side::Buy { "LONG" } else { "SHORT" },
            (-payload.pnl) as f64 / 1_000_000.0
        );

        match payload.side {
            Side::Buy => {
                self.long_position_size = 0;
            }
            Side::Sell => {
                self.short_position_size = 0;
            }
        }
        self.collateral_locked = (self.collateral_locked - payload.collateral_lost).max(0);
    }

    fn handle_market_state(&mut self, payload: &MarketStatePayload) {
        if payload.symbol == self.symbol {
            self.oi_long_usd = payload.oi_long_usd;
            self.oi_short_usd = payload.oi_short_usd;
        }
    }

    fn execute_strategy(&mut self, sim: &mut dyn SimulatorApi) {
        // Priority 1: Seed liquidity if market is empty
        if let Some(side) = self.needs_seed_liquidity() {
            self.open_position(sim, side, "SEED");
            self.rebalance_actions += 1;
            return;
        }

        // Priority 2: Rebalance if imbalanced
        if let Some(side) = self.needs_rebalancing() {
            // Don't open more if we already have a big position on this side
            let existing = match side {
                Side::Buy => self.long_position_size,
                Side::Sell => self.short_position_size,
            };

            // Limit our exposure per side
            let max_exposure = self.target_oi_per_side / 2;
            if existing < max_exposure {
                self.open_position(sim, side, "REBALANCE");
                self.rebalance_actions += 1;
            }
        }
    }
}

impl Agent for MarketMakerAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn on_start(&mut self, sim: &mut dyn SimulatorApi) {
        println!(
            "[MM {}] Starting: target_oi=${:.0}k/side, max_imbalance={:.0}%, order_size={:.1} tokens",
            self.name,
            self.target_oi_per_side as f64 / 1_000_000_000.0,
            self.max_imbalance_pct,
            self.order_size_tokens
        );

        // Schedule first wakeup
        sim.wakeup(self.id, sim.now_ns() + self.wake_interval_ns);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        self.execute_strategy(sim);
        sim.wakeup(self.id, now_ns + self.wake_interval_ns);
    }

    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, msg: &Message) {
        match msg.msg_type {
            MessageType::OracleTick => {
                if let MessagePayload::OracleTick(OracleTickPayload { symbol, price, .. }) = &msg.payload {
                    if *symbol == self.symbol {
                        self.current_price = Some((price.min + price.max) / 2);
                    }
                }
            }
            MessageType::MarketState => {
                if let MessagePayload::MarketState(p) = &msg.payload {
                    self.handle_market_state(p);
                }
            }
            MessageType::OrderExecuted => {
                if let MessagePayload::OrderExecuted(p) = &msg.payload {
                    if p.symbol == self.symbol {
                        self.handle_order_executed(p);
                    }
                }
            }
            MessageType::PositionLiquidated => {
                if let MessagePayload::PositionLiquidated(p) = &msg.payload {
                    if p.symbol == self.symbol {
                        self.handle_liquidation(p);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        let imbalance = self.calculate_imbalance_pct();
        println!(
            "[MM {}] STOP: orders={} rebalances={} final_imbalance={:.1}% bal=${:.0}k",
            self.name,
            self.orders_placed,
            self.rebalance_actions,
            imbalance,
            self.balance as f64 / 1_000_000_000.0
        );
    }
}
