//! Smart Trader Agent - different trading strategies for testing the perp-futures engine
//!
//! Strategies:
//! - Hodler: Opens a position and holds for extended time (tests borrowing/funding fees)
//! - Risky: High leverage trader, likely to be liquidated with price movement
//! - TrendFollower: Trades based on recent price momentum

use crate::agents::Agent;
use crate::messages::{
    AgentId, CloseOrderPayload, MarketOrderPayload, Message, MessagePayload, MessageType,
    OracleTickPayload, Side, SimulatorApi,
};
use std::collections::VecDeque;

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
    pub qty: u64,
    pub wake_interval_ms: u64,
}

/// Smart trader with configurable strategies
pub struct SmartTraderAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    symbol: String,
    strategy: TradingStrategy,
    qty: u64,
    wake_interval_ns: u64,

    // Position tracking
    has_position: bool,
    position_side: Option<Side>,
    position_opened_at: u64,

    // Price tracking (for trend following)
    price_history: VecDeque<(u64, u64)>, // (timestamp_ns, price)
    current_price: Option<u64>,

    // Stats
    trades_opened: u32,
    trades_closed: u32,
}

impl SmartTraderAgent {
    pub fn new(id: AgentId, config: SmartTraderConfig) -> Self {
        Self {
            id,
            name: config.name,
            exchange_id: config.exchange_id,
            symbol: config.symbol,
            strategy: config.strategy,
            qty: config.qty,
            wake_interval_ns: config.wake_interval_ms * 1_000_000,
            has_position: false,
            position_side: None,
            position_opened_at: 0,
            price_history: VecDeque::with_capacity(100),
            current_price: None,
            trades_opened: 0,
            trades_closed: 0,
        }
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
        let payload = MessagePayload::MarketOrder(MarketOrderPayload {
            symbol: self.symbol.clone(),
            side,
            qty: self.qty,
            leverage,
        });

        let side_str = match side {
            Side::Buy => "LONG",
            Side::Sell => "SHORT",
        };

        println!(
            "[SmartTrader {}] OPEN {} {}x qty={}",
            self.name, side_str, leverage, self.qty
        );

        sim.send(
            self.id,
            self.exchange_id,
            MessageType::MarketOrder,
            payload,
        );

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

            println!("[SmartTrader {}] CLOSE {}", self.name, side_str);

            sim.send(
                self.id,
                self.exchange_id,
                MessageType::CloseOrder,
                payload,
            );

            self.has_position = false;
            self.position_side = None;
            self.trades_closed += 1;
        }
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
            let side = if self.trades_opened % 2 == 0 {
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
                    println!(
                        "[SmartTrader {}] Momentum: {:.2}% -> LONG",
                        self.name, change_pct
                    );
                    self.open_position(sim, Side::Buy, now_ns);
                } else if change_pct < -*threshold_pct {
                    // Price going down -> Short
                    println!(
                        "[SmartTrader {}] Momentum: {:.2}% -> SHORT",
                        self.name, change_pct
                    );
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
        // Listen to oracle ticks to track prices
        if let MessageType::OracleTick = msg.msg_type {
            if let MessagePayload::OracleTick(OracleTickPayload { symbol, price, .. }) = &msg.payload
            {
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
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        let strategy_name = match &self.strategy {
            TradingStrategy::Hodler { leverage, .. } => format!("Hodler({}x)", leverage),
            TradingStrategy::Risky { leverage } => format!("Risky({}x)", leverage),
            TradingStrategy::TrendFollower { leverage, .. } => format!("TrendFollower({}x)", leverage),
        };

        println!(
            "[SmartTrader {}] stopping. Strategy={}, opened={}, closed={}, has_position={}",
            self.name, strategy_name, self.trades_opened, self.trades_closed, self.has_position
        );
    }
}

