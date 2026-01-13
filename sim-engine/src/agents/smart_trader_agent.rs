//! Smart Trader Agent - different trading strategies for testing the perp-futures engine
//!
//! Strategies:
//! - Hodler: Opens a position and holds for extended time (tests borrowing/funding fees)
//! - Risky: High leverage trader, likely to be liquidated with price movement
//! - TrendFollower: Trades based on recent price momentum

use crate::agents::Agent;
use crate::messages::{
    AgentId, CloseOrderPayload, MarketOrderPayload, Message, MessagePayload, MessageType,
    OrderExecutedPayload, OrderExecutionType, OracleTickPayload, PositionLiquidatedPayload, Side,
    SimulatorApi,
};
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

/// Default starting balance for each trader (in micro-USD = $10,000)
const DEFAULT_BALANCE: i128 = 10_000_000_000;

/// Trading strategy configuration
#[derive(Debug, Clone)]
pub enum TradingStrategy {
    /// Opens position and holds for specified duration
    Hodler {
        side: Side,
        hold_duration_sec: u64,
        leverage: u32,
    },
    /// High leverage, high risk trading
    Risky {
        leverage: u32, // 10x, 20x, 50x
    },
    /// Follows price momentum
    TrendFollower {
        lookback_sec: u64,
        threshold_pct: f64, // e.g., 0.5 = 0.5% move triggers trade
        leverage: u32,
    },
}

/// Configuration for SmartTraderAgent
#[derive(Debug, Clone)]
pub struct SmartTraderConfig {
    pub name: String,
    pub exchange_id: AgentId,
    pub symbol: String,
    pub strategy: TradingStrategy,
    pub qty_min: f64,
    pub qty_max: f64,
    pub wake_interval_ms: u64,
}

/// Smart trader with configurable strategies
pub struct SmartTraderAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    symbol: String,
    strategy: TradingStrategy,
    qty_min: f64,
    qty_max: f64,
    wake_interval_ns: u64,

    // Position tracking
    has_position: bool,
    position_side: Option<Side>,
    position_opened_at: u64,

    // Balance tracking (micro-USD)
    balance: i128,
    collateral_in_position: i128, // How much is locked in current position

    // Price tracking (for trend following)
    price_history: VecDeque<(u64, u64)>, // (timestamp_ns, price)
    current_price: Option<u64>,

    // Stats
    trades_opened: u32,
    trades_closed: u32,
    liquidations: u32,
    total_pnl: i128,
}

impl SmartTraderAgent {
    pub fn new(id: AgentId, config: SmartTraderConfig) -> Self {
        Self {
            id,
            name: config.name,
            exchange_id: config.exchange_id,
            symbol: config.symbol,
            strategy: config.strategy,
            qty_min: config.qty_min,
            qty_max: config.qty_max.max(config.qty_min),
            wake_interval_ns: config.wake_interval_ms * 1_000_000,
            has_position: false,
            position_side: None,
            position_opened_at: 0,
            balance: DEFAULT_BALANCE,
            collateral_in_position: 0,
            price_history: VecDeque::with_capacity(100),
            current_price: None,
            trades_opened: 0,
            trades_closed: 0,
            liquidations: 0,
            total_pnl: 0,
        }
    }

    /// Generate random qty within configured range using timestamp as seed
    /// Returns quantity in tokens as f64 (e.g. 0.5 = half a token)
    fn random_qty(&self, now_ns: u64) -> f64 {
        if (self.qty_max - self.qty_min).abs() < 0.001 {
            return self.qty_min.max(0.01);
        }
        // Simple hash-based randomization using timestamp + trader id
        let mut hasher = DefaultHasher::new();
        (now_ns, self.id, self.trades_opened).hash(&mut hasher);
        let hash = hasher.finish();

        // Generate random float between qty_min and qty_max
        let range = self.qty_max - self.qty_min;
        let fraction = (hash % 1000) as f64 / 1000.0; // 0.0 - 0.999
        self.qty_min + (range * fraction)
    }

    /// Check if trader has enough balance to open a position
    fn can_afford(&self, collateral_needed: i128) -> bool {
        self.balance >= collateral_needed
    }

    fn get_leverage(&self) -> u32 {
        match &self.strategy {
            TradingStrategy::Hodler { leverage, .. } => *leverage,
            TradingStrategy::Risky { leverage } => *leverage,
            TradingStrategy::TrendFollower { leverage, .. } => *leverage,
        }
    }

    fn open_position(&mut self, sim: &mut dyn SimulatorApi, side: Side, now_ns: u64) {
        let leverage = self.get_leverage();
        let qty_tokens = self.random_qty(now_ns); // in tokens as f64 (e.g. 0.5)
        // qty_tokens = tokens as float, current_price = micro-USD per token
        // size = qty_tokens * price (in micro-USD)
        let price_micro = self.current_price.unwrap_or(1_000_000) as i128; // Default $1 if no price
        let size_micro = (qty_tokens * price_micro as f64) as i128; // size in micro-USD
        let collateral_needed = size_micro / self.get_leverage() as i128;

        // Check if we can afford it
        if !self.can_afford(collateral_needed) {
            println!(
                "[SmartTrader {}] SKIP OPEN - insufficient balance: ${:.2} < ${:.2}",
                self.name,
                self.balance as f64 / 1_000_000.0,
                collateral_needed as f64 / 1_000_000.0
            );
            return;
        }

        let payload = MessagePayload::MarketOrder(MarketOrderPayload {
            symbol: self.symbol.clone(),
            side,
            qty: qty_tokens,
            leverage,
        });

        let side_str = match side {
            Side::Buy => "LONG",
            Side::Sell => "SHORT",
        };

        println!(
            "[SmartTrader {}] OPEN {} {}x qty={:.2} tokens (balance: ${:.2})",
            self.name,
            side_str,
            leverage,
            qty_tokens,
            self.balance as f64 / 1_000_000.0
        );

        sim.send(self.id, self.exchange_id, MessageType::MarketOrder, payload);

        // Lock collateral
        self.balance -= collateral_needed;
        self.collateral_in_position = collateral_needed;

        self.has_position = true;
        self.position_side = Some(side);
        self.position_opened_at = now_ns;
        self.trades_opened += 1;
    }

    fn close_position(&mut self, sim: &mut dyn SimulatorApi) {
        if let Some(side) = self.position_side {
            let payload = MessagePayload::CloseOrder(CloseOrderPayload {
                symbol: self.symbol.clone(),
                side,
            });

            let side_str = match side {
                Side::Buy => "LONG",
                Side::Sell => "SHORT",
            };

            println!(
                "[SmartTrader {}] CLOSE {} (balance before return: ${:.2})",
                self.name,
                side_str,
                self.balance as f64 / 1_000_000.0
            );

            sim.send(self.id, self.exchange_id, MessageType::CloseOrder, payload);

            // Note: Don't update balance here - wait for OrderExecuted with actual PnL
            // Position state will be updated when we receive OrderExecuted
            self.has_position = false;
            self.position_side = None;
            self.trades_closed += 1;
        }
    }

    /// Handle order execution notification from exchange
    fn handle_order_executed(&mut self, payload: &OrderExecutedPayload) {
        match payload.order_type {
            OrderExecutionType::Increase => {
                // Real collateral locked (may differ from our estimate due to fees/slippage)
                let actual_collateral = payload.collateral_delta;
                // Adjust balance: we estimated collateral_in_position, but actual may differ
                self.balance += self.collateral_in_position; // Return our estimate
                self.balance -= actual_collateral;           // Deduct actual
                self.collateral_in_position = actual_collateral;
            }
            OrderExecutionType::Decrease => {
                // Position closed - return collateral and apply PnL
                self.balance += self.collateral_in_position; // Return collateral
                self.balance += payload.pnl;                  // Apply PnL
                self.total_pnl += payload.pnl;
                self.collateral_in_position = 0;
            }
            OrderExecutionType::Liquidation => {
                // Liquidation handled in handle_liquidation
            }
        }
    }

    /// Handle notification that our position was liquidated
    fn handle_liquidation(&mut self, payload: &PositionLiquidatedPayload) {
        println!(
            "[SmartTrader {}] ⚠️ LIQUIDATED {} size=${:.2} pnl=${:.2} lost=${:.2}",
            self.name,
            match payload.side {
                Side::Buy => "LONG",
                Side::Sell => "SHORT",
            },
            payload.size_usd as f64 / 1_000_000.0,
            payload.pnl as f64 / 1_000_000.0,
            payload.collateral_lost as f64 / 1_000_000.0
        );

        // Lost our collateral
        self.collateral_in_position = 0;
        self.has_position = false;
        self.position_side = None;
        self.liquidations += 1;
        self.total_pnl += payload.pnl;

        println!(
            "[SmartTrader {}] Balance after liquidation: ${:.2} (total PnL: ${:.2})",
            self.name,
            self.balance as f64 / 1_000_000.0,
            self.total_pnl as f64 / 1_000_000.0
        );
    }

    fn execute_hodler_strategy(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        if let TradingStrategy::Hodler {
            side,
            hold_duration_sec,
            ..
        } = &self.strategy
        {
            let hold_duration_ns = *hold_duration_sec * 1_000_000_000;

            if !self.has_position {
                // Open position if we don't have one
                if self.current_price.is_some() {
                    self.open_position(sim, *side, now_ns);
                }
            } else {
                // Check if hold duration passed
                let held_for = now_ns.saturating_sub(self.position_opened_at);
                if held_for >= hold_duration_ns {
                    println!(
                        "[SmartTrader {}] Held position for {}s, closing",
                        self.name,
                        held_for / 1_000_000_000
                    );
                    self.close_position(sim);
                }
            }
        }
    }

    fn execute_risky_strategy(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        // Risky trader: opens position and holds until something happens
        // With high leverage, likely to be liquidated on small price moves
        if !self.has_position && self.current_price.is_some() {
            // Randomly choose side (alternating for variety)
            let side = if self.trades_opened.is_multiple_of(2) {
                Side::Buy
            } else {
                Side::Sell
            };
            self.open_position(sim, side, now_ns);
        }
        // Don't close - wait for liquidation or manual intervention
    }

    fn execute_trend_follower_strategy(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        if let TradingStrategy::TrendFollower {
            lookback_sec,
            threshold_pct,
            ..
        } = &self.strategy
        {
            let lookback_ns = *lookback_sec * 1_000_000_000;

            // Need at least 2 prices
            if self.price_history.len() < 2 {
                return;
            }

            // Get old price from lookback period
            let cutoff_time = now_ns.saturating_sub(lookback_ns);
            let old_price = self
                .price_history
                .iter()
                .find(|(ts, _)| *ts >= cutoff_time)
                .map(|(_, p)| *p);

            let current = match self.current_price {
                Some(p) => p,
                None => return,
            };

            let old = match old_price {
                Some(p) => p,
                None => return,
            };

            // Calculate momentum
            let change_pct = (current as f64 - old as f64) / old as f64 * 100.0;

            if !self.has_position {
                // Open position based on momentum
                if change_pct > *threshold_pct {
                    // Price going up -> Long
                    println!("[SmartTrader {}] Momentum: {:.2}% -> LONG", self.name, change_pct);
                    self.open_position(sim, Side::Buy, now_ns);
                } else if change_pct < -*threshold_pct {
                    // Price going down -> Short
                    println!("[SmartTrader {}] Momentum: {:.2}% -> SHORT", self.name, change_pct);
                    self.open_position(sim, Side::Sell, now_ns);
                }
            } else {
                // Close if momentum reverses
                if let Some(side) = self.position_side {
                    let should_close = match side {
                        Side::Buy => change_pct < -*threshold_pct / 2.0, // Close long if price dropping
                        Side::Sell => change_pct > *threshold_pct / 2.0, // Close short if price rising
                    };
                    if should_close {
                        println!(
                            "[SmartTrader {}] Momentum reversed: {:.2}% -> CLOSE",
                            self.name, change_pct
                        );
                        self.close_position(sim);
                    }
                }
            }
        }
    }
}

impl Agent for SmartTraderAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn on_start(&mut self, sim: &mut dyn SimulatorApi) {
        let strategy_name = match &self.strategy {
            TradingStrategy::Hodler { side, leverage, .. } => {
                format!("Hodler({:?}, {}x)", side, leverage)
            }
            TradingStrategy::Risky { leverage } => format!("Risky({}x)", leverage),
            TradingStrategy::TrendFollower { leverage, .. } => {
                format!("TrendFollower({}x)", leverage)
            }
        };

        println!(
            "[SmartTrader {}] starting -> exchange={}, symbol={}, strategy={}",
            self.name, self.exchange_id, self.symbol, strategy_name
        );

        // Schedule first wakeup after a delay to let prices settle
        let first_wake = sim.now_ns().saturating_add(self.wake_interval_ns * 2);
        sim.wakeup(self.id, first_wake);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        match &self.strategy {
            TradingStrategy::Hodler { .. } => self.execute_hodler_strategy(sim, now_ns),
            TradingStrategy::Risky { .. } => self.execute_risky_strategy(sim, now_ns),
            TradingStrategy::TrendFollower { .. } => self.execute_trend_follower_strategy(sim, now_ns),
        }

        // Schedule next wakeup
        let next = now_ns.saturating_add(self.wake_interval_ns);
        sim.wakeup(self.id, next);
    }

    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, msg: &Message) {
        match msg.msg_type {
            // Listen to oracle ticks to track prices
            MessageType::OracleTick => {
                if let MessagePayload::OracleTick(OracleTickPayload { symbol, price, .. }) = &msg.payload {
                    if *symbol == self.symbol {
                        let mid_price = (price.min + price.max) / 2;
                        self.current_price = Some(mid_price);

                        // Store in history (keep last 100 prices)
                        self.price_history.push_back((msg.at, mid_price));
                        if self.price_history.len() > 100 {
                            self.price_history.pop_front();
                        }
                    }
                }
            }
            // Handle liquidation notification
            MessageType::PositionLiquidated => {
                if let MessagePayload::PositionLiquidated(payload) = &msg.payload {
                    if payload.symbol == self.symbol {
                        self.handle_liquidation(payload);
                    }
                }
            }
            // Handle order execution (for accurate balance tracking)
            MessageType::OrderExecuted => {
                if let MessagePayload::OrderExecuted(payload) = &msg.payload {
                    if payload.symbol == self.symbol {
                        self.handle_order_executed(payload);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        let strategy_name = match &self.strategy {
            TradingStrategy::Hodler { leverage, .. } => format!("Hodler({}x)", leverage),
            TradingStrategy::Risky { leverage } => format!("Risky({}x)", leverage),
            TradingStrategy::TrendFollower { leverage, .. } => format!("TrendFollower({}x)", leverage),
        };

        let pnl_str = if self.total_pnl >= 0 {
            format!("+${:.2}", self.total_pnl as f64 / 1_000_000.0)
        } else {
            format!("-${:.2}", (-self.total_pnl) as f64 / 1_000_000.0)
        };

        println!(
            "[SmartTrader {}] FINAL: strategy={}, opened={}, closed={}, liquidated={}, pnl={}, balance=${:.2}",
            self.name,
            strategy_name,
            self.trades_opened,
            self.trades_closed,
            self.liquidations,
            pnl_str,
            self.balance as f64 / 1_000_000.0
        );
    }
}
