use crate::agents::Agent;
use crate::messages::{
    AgentId, CloseOrderPayload, ExecutionType, MarketOrderPayload, MarketStatePayload, Message, MessagePayload,
    MessageType, OracleTickPayload, OrderExecutedPayload, OrderExecutionType, OrderPayload, OrderType,
    PositionLiquidatedPayload, Side, SimulatorApi,
};
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

const DEFAULT_BALANCE: i128 = 50_000_000_000; // $50,000
const MAX_COLLATERAL_FRACTION: f64 = 0.30; // cap per trade to 30% of available balance
const MIN_QTY_TOKENS: f64 = 0.01;

// === NEW: OI-aware safety constants ===
/// Minimum total OI before allowing new positions (market warmup period)
const MIN_MARKET_OI: i128 = 20_000_000_000; // $20k minimum total OI

/// Maximum allowed imbalance to open position that makes imbalance worse
const MAX_IMBALANCE_FOR_AGGRAVATING: f64 = 40.0; // 40% imbalance = danger zone

#[derive(Debug, Clone)]
pub enum TradingStrategy {
    Hodler {
        side: Side,
        hold_duration_sec: u64,
        leverage: u32,
        take_profit_pct: Option<f64>,
        stop_loss_pct: Option<f64>,
    },
    Institutional {
        side: Side,
        leverage: u32,
        take_profit_pct: f64,
        stop_loss_pct: f64,
        min_hold_sec: u64,
        max_hold_sec: u64,
        reentry_delay_sec: u64,
    },
    TrendFollower {
        lookback_sec: u64,
        threshold_pct: f64,
        leverage: u32,
        take_profit_pct: Option<f64>,
        stop_loss_pct: Option<f64>,
    },
    MeanReversion {
        lookback_periods: u32,
        entry_deviation_pct: f64,
        exit_deviation_pct: f64,
        leverage: u32,
        max_hold_sec: u64,
    },
    /// Arbitrageur: opens position against OI imbalance to capture price impact bonus
    /// NOTE: This strategy BYPASSES OI safety checks because its purpose IS to trade during imbalance
    Arbitrageur {
        min_imbalance_pct: f64,
        leverage: u32,
        hold_duration_sec: u64,
        take_profit_pct: Option<f64>,
        stop_loss_pct: Option<f64>,
    },
    /// FundingHarvester: sits on the smaller side of OI to receive funding payments
    /// NOTE: This strategy BYPASSES OI safety checks because its purpose IS to trade during imbalance
    FundingHarvester {
        min_imbalance_pct: f64,
        leverage: u32,
        min_hold_sec: u64,
        max_hold_sec: u64,
        exit_imbalance_pct: f64,
        stop_loss_pct: f64,
    },
}

impl TradingStrategy {
    /// Returns true if this strategy should bypass OI safety checks
    /// (Arbitrageur and FundingHarvester exist TO trade during imbalance)
    fn bypasses_oi_checks(&self) -> bool {
        matches!(
            self,
            TradingStrategy::Arbitrageur { .. } | TradingStrategy::FundingHarvester { .. }
        )
    }
}

#[derive(Debug, Clone)]
pub struct SmartTraderConfig {
    pub name: String,
    pub exchange_id: AgentId,
    pub symbol: String,
    pub address: Option<String>,
    pub strategy: TradingStrategy,
    pub qty_min: f64,
    pub qty_max: f64,
    pub wake_interval_ms: u64,
    pub balance: Option<i128>,
    /// NEW: Delay before first trade (for staggered start)
    pub start_delay_ms: Option<u64>,
}

pub struct SmartTraderAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    symbol: String,
    address: Option<String>,
    strategy: TradingStrategy,
    qty_min: f64,
    qty_max: f64,
    wake_interval_ns: u64,
    start_delay_ns: u64, // NEW: staggered start

    has_position: bool,
    position_side: Option<Side>,
    position_opened_at: u64,
    entry_price: Option<u64>,
    last_close_at: u64,

    balance: i128,
    collateral_in_position: i128,

    price_history: VecDeque<(u64, u64)>,
    current_price: Option<u64>,

    // OI tracking for Arbitrageur/FundingHarvester
    oi_long_usd: i128,
    oi_short_usd: i128,
    liquidity_usd: i128, // NEW: track liquidity

    trades_opened: u32,
    trades_closed: u32,
    liquidations: u32,
    total_pnl: i128,

    skipped_due_to_oi: u32,

    pending_sl_order_id: Option<u64>,
    pending_tp_order_id: Option<u64>,
    use_conditional_sl_tp: bool,
}

impl SmartTraderAgent {
    pub fn new(id: AgentId, config: SmartTraderConfig) -> Self {
        // Calculate staggered start delay
        // Base delay + spread based on agent ID
        let base_delay = config.start_delay_ms.unwrap_or(0) * 1_000_000;
        let id_spread = (id as u64 % 30) * 200_000_000; // 0-6 seconds spread
        let start_delay_ns = base_delay + id_spread;

        Self {
            id,
            name: config.name,
            exchange_id: config.exchange_id,
            symbol: config.symbol,
            address: config.address,
            strategy: config.strategy,
            qty_min: config.qty_min,
            qty_max: config.qty_max.max(config.qty_min),
            wake_interval_ns: config.wake_interval_ms * 1_000_000,
            start_delay_ns,
            has_position: false,
            position_side: None,
            position_opened_at: 0,
            entry_price: None,
            last_close_at: 0,
            balance: config.balance.unwrap_or(DEFAULT_BALANCE),
            collateral_in_position: 0,
            price_history: VecDeque::with_capacity(200),
            current_price: None,
            oi_long_usd: 0,
            oi_short_usd: 0,
            liquidity_usd: 0,
            trades_opened: 0,
            trades_closed: 0,
            liquidations: 0,
            total_pnl: 0,
            skipped_due_to_oi: 0,
            pending_sl_order_id: None,
            pending_tp_order_id: None,
            use_conditional_sl_tp: true,
        }
    }

    pub fn set_address(&mut self, address: String) {
        self.address = Some(address);
    }

    fn random_qty(&self, now_ns: u64) -> f64 {
        if (self.qty_max - self.qty_min).abs() < 0.001 {
            return self.qty_min.max(0.01);
        }
        let mut hasher = DefaultHasher::new();
        (now_ns, self.id, self.trades_opened).hash(&mut hasher);
        let hash = hasher.finish();
        let range = self.qty_max - self.qty_min;
        let fraction = (hash % 1000) as f64 / 1000.0;
        self.qty_min + (range * fraction)
    }

    fn get_leverage(&self) -> u32 {
        match &self.strategy {
            TradingStrategy::Hodler { leverage, .. } => *leverage,
            TradingStrategy::Institutional { leverage, .. } => *leverage,
            TradingStrategy::TrendFollower { leverage, .. } => *leverage,
            TradingStrategy::MeanReversion { leverage, .. } => *leverage,
            TradingStrategy::Arbitrageur { leverage, .. } => *leverage,
            TradingStrategy::FundingHarvester { leverage, .. } => *leverage,
        }
    }

    fn calculate_pnl_pct(&self) -> Option<f64> {
        let entry = self.entry_price? as f64;
        let current = self.current_price? as f64;
        let side = self.position_side?;
        Some(match side {
            Side::Buy => (current - entry) / entry * 100.0,
            Side::Sell => (entry - current) / entry * 100.0,
        })
    }

    fn calculate_sma(&self, periods: u32) -> Option<f64> {
        if self.price_history.len() < periods as usize {
            return None;
        }
        let sum: u64 = self
            .price_history
            .iter()
            .rev()
            .take(periods as usize)
            .map(|(_, price)| *price)
            .sum();
        Some(sum as f64 / periods as f64)
    }

    /// Calculate OI imbalance percentage
    /// Positive = long-heavy, Negative = short-heavy
    fn calculate_oi_imbalance_pct(&self) -> f64 {
        let total = self.oi_long_usd + self.oi_short_usd;
        if total == 0 {
            return 0.0;
        }
        let long = self.oi_long_usd as f64;
        let short = self.oi_short_usd as f64;
        (long - short) / total as f64 * 100.0
    }

    /// Get the smaller side of OI (the one receiving funding)
    fn get_smaller_oi_side(&self) -> Option<Side> {
        if self.oi_long_usd == 0 && self.oi_short_usd == 0 {
            return None;
        }
        if self.oi_long_usd <= self.oi_short_usd {
            Some(Side::Buy) // longs are smaller, they receive funding
        } else {
            Some(Side::Sell) // shorts are smaller
        }
    }

    /// NEW: Check if it's safe to open a position based on market conditions
    /// Returns (is_safe, reason)
    fn is_safe_to_open(&self, side: Side, now_ns: u64) -> (bool, &'static str) {
        // Arbitrageur and FundingHarvester bypass these checks
        if self.strategy.bypasses_oi_checks() {
            return (true, "balancer_strategy");
        }

        let total_oi = self.oi_long_usd + self.oi_short_usd;
        let imbalance = self.calculate_oi_imbalance_pct();

        // 1. Emergency check: is one side completely empty?
        if self.oi_long_usd == 0 || self.oi_short_usd == 0 {
            // Record when we saw empty side
            if total_oi > 0 {
                // One side is empty - be very careful
                let joining_empty = match side {
                    Side::Buy => self.oi_long_usd == 0,
                    Side::Sell => self.oi_short_usd == 0,
                };

                if joining_empty {
                    // We're joining the empty side - this is GOOD (helping balance)
                    return (true, "joining_empty_side");
                } else {
                    // We're piling onto the existing side - this is BAD
                    return (false, "would_worsen_empty");
                }
            }
        }

        // 2. Warmup period check
        if total_oi < MIN_MARKET_OI {
            // During warmup, prefer joining the smaller side
            let joining_smaller = match side {
                Side::Buy => self.oi_long_usd <= self.oi_short_usd,
                Side::Sell => self.oi_short_usd <= self.oi_long_usd,
            };

            if joining_smaller || total_oi == 0 {
                return (true, "warmup_ok");
            } else {
                // Allow with lower probability during warmup
                let allow_anyway = (now_ns % 100) < 30; // 30% chance
                if allow_anyway {
                    return (true, "warmup_allowed");
                }
                return (false, "warmup_wrong_side");
            }
        }

        // 3. Imbalance check - don't make imbalance worse when it's bad
        let would_worsen = match side {
            Side::Buy => imbalance > 0.0,  // Long-heavy, adding long makes it worse
            Side::Sell => imbalance < 0.0, // Short-heavy, adding short makes it worse
        };

        if would_worsen && imbalance.abs() > MAX_IMBALANCE_FOR_AGGRAVATING {
            return (false, "would_worsen_imbalance");
        }

        (true, "ok")
    }

    fn open_position(&mut self, sim: &mut dyn SimulatorApi, side: Side, now_ns: u64) {
        // OI safety check
        let (is_safe, reason) = self.is_safe_to_open(side, now_ns);
        if !is_safe {
            self.skipped_due_to_oi += 1;
            if self.skipped_due_to_oi <= 3 || self.skipped_due_to_oi % 20 == 0 {
                println!(
                    "[{}] SKIP {} - {} (OI: L=${:.0}k S=${:.0}k, imb={:.1}%)",
                    self.name,
                    if side == Side::Buy { "LONG" } else { "SHORT" },
                    reason,
                    self.oi_long_usd as f64 / 1_000_000_000.0,
                    self.oi_short_usd as f64 / 1_000_000_000.0,
                    self.calculate_oi_imbalance_pct()
                );
            }
            return;
        }

        let leverage = self.get_leverage();
        let price_micro = self.current_price.unwrap_or(1_000_000) as f64;
        let max_collateral = (self.balance as f64) * MAX_COLLATERAL_FRACTION;
        if max_collateral <= 0.0 {
            return;
        }
        let max_size_micro = max_collateral * leverage as f64;
        let max_qty = max_size_micro / price_micro.max(1.0);
        if max_qty < MIN_QTY_TOKENS {
            return;
        }

        let mut qty_tokens = self.random_qty(now_ns);
        if qty_tokens > max_qty {
            qty_tokens = max_qty;
        }

        let size_micro = (qty_tokens * price_micro) as i128;
        let collateral_needed = size_micro / leverage as i128;

        if collateral_needed <= 0 || self.balance < collateral_needed {
            return;
        }

        println!(
            "[{}] OPEN {} {}x qty={:.3} @ ${:.2} (OI: L=${:.0}k S=${:.0}k)",
            self.name,
            if side == Side::Buy { "LONG" } else { "SHORT" },
            leverage,
            qty_tokens,
            price_micro as f64 / 1_000_000.0,
            self.oi_long_usd as f64 / 1_000_000_000.0,
            self.oi_short_usd as f64 / 1_000_000_000.0
        );

        // Send MarketOrder message to Exchange (no blockchain knowledge)
        sim.send(
            self.id,
            self.exchange_id,
            MessageType::MarketOrder,
            MessagePayload::MarketOrder(MarketOrderPayload {
                symbol: self.symbol.clone(),
                side,
                qty: qty_tokens,
                leverage,
            }),
        );

        // Local tracking (will be updated on OrderExecuted)
        self.balance -= collateral_needed;
        self.collateral_in_position = collateral_needed;
        self.has_position = true;
        self.position_side = Some(side);
        self.position_opened_at = now_ns;
        self.entry_price = self.current_price;
        self.trades_opened += 1;
    }

    fn close_position(&mut self, sim: &mut dyn SimulatorApi, reason: &str, _now_ns: u64) {
        if let Some(side) = self.position_side {
            let pnl_pct = self.calculate_pnl_pct().unwrap_or(0.0);

            println!(
                "[{}] CLOSE {} ({}) pnl={:+.2}%",
                self.name,
                if side == Side::Buy { "LONG" } else { "SHORT" },
                reason,
                pnl_pct
            );

            // Send CloseOrder message to Exchange (no blockchain knowledge)
            sim.send(
                self.id,
                self.exchange_id,
                MessageType::CloseOrder,
                MessagePayload::CloseOrder(CloseOrderPayload {
                    symbol: self.symbol.clone(),
                    side,
                }),
            );

            // Local tracking (will be finalized on OrderExecuted)
            self.has_position = false;
            self.position_side = None;
            self.entry_price = None;
            self.last_close_at = sim.now_ns();
            self.trades_closed += 1;
            self.pending_sl_order_id = None;
            self.pending_tp_order_id = None;
        }
    }

    fn submit_sl_tp_orders(&mut self, sim: &mut dyn SimulatorApi, entry_price: u64) {
        let (sl_pct, tp_pct) = match &self.strategy {
            TradingStrategy::Hodler {
                stop_loss_pct,
                take_profit_pct,
                ..
            } => (stop_loss_pct.unwrap_or(0.0), take_profit_pct.unwrap_or(0.0)),
            TradingStrategy::Institutional {
                stop_loss_pct,
                take_profit_pct,
                ..
            } => (*stop_loss_pct, *take_profit_pct),
            TradingStrategy::TrendFollower {
                stop_loss_pct,
                take_profit_pct,
                ..
            } => (stop_loss_pct.unwrap_or(0.0), take_profit_pct.unwrap_or(0.0)),
            TradingStrategy::MeanReversion { .. } => (0.0, 0.0),
            TradingStrategy::Arbitrageur {
                stop_loss_pct,
                take_profit_pct,
                ..
            } => (stop_loss_pct.unwrap_or(0.0), take_profit_pct.unwrap_or(0.0)),
            TradingStrategy::FundingHarvester { stop_loss_pct, .. } => (*stop_loss_pct, 0.0),
        };

        let side = match self.position_side {
            Some(s) => s,
            None => return,
        };

        if sl_pct > 0.0 {
            let sl_price = match side {
                Side::Buy => ((entry_price as f64) * (1.0 - sl_pct / 100.0)) as u64,
                Side::Sell => ((entry_price as f64) * (1.0 + sl_pct / 100.0)) as u64,
            };

            let sl_order = OrderPayload {
                symbol: self.symbol.clone(),
                side,
                order_type: OrderType::Decrease,
                execution_type: ExecutionType::StopLoss,
                qty: None,
                leverage: None,
                size_delta_usd: None,
                trigger_price: Some(sl_price),
                acceptable_price: None,
                valid_for_sec: Some(86400),
            };

            sim.send(
                self.id,
                self.exchange_id,
                MessageType::SubmitOrder,
                MessagePayload::Order(sl_order),
            );
        }

        if tp_pct > 0.0 {
            let tp_price = match side {
                Side::Buy => ((entry_price as f64) * (1.0 + tp_pct / 100.0)) as u64,
                Side::Sell => ((entry_price as f64) * (1.0 - tp_pct / 100.0)) as u64,
            };

            let tp_order = OrderPayload {
                symbol: self.symbol.clone(),
                side,
                order_type: OrderType::Decrease,
                execution_type: ExecutionType::TakeProfit,
                qty: None,
                leverage: None,
                size_delta_usd: None,
                trigger_price: Some(tp_price),
                acceptable_price: None,
                valid_for_sec: Some(86400),
            };

            sim.send(
                self.id,
                self.exchange_id,
                MessageType::SubmitOrder,
                MessagePayload::Order(tp_order),
            );
        }
    }

    fn handle_order_executed(&mut self, sim: &mut dyn SimulatorApi, payload: &OrderExecutedPayload) {
        match payload.order_type {
            OrderExecutionType::Increase => {
                let actual = payload.collateral_delta;
                self.balance += self.collateral_in_position;
                self.balance -= actual;
                self.collateral_in_position = actual;

                if self.use_conditional_sl_tp {
                    if let Some(price) = self.current_price {
                        self.submit_sl_tp_orders(sim, price);
                    }
                }
            }
            OrderExecutionType::Decrease => {
                self.balance += self.collateral_in_position;
                self.balance += payload.pnl;
                self.total_pnl += payload.pnl;
                self.collateral_in_position = 0;
                self.pending_sl_order_id = None;
                self.pending_tp_order_id = None;
            }
            OrderExecutionType::Liquidation => {}
        }
    }

    fn handle_liquidation(&mut self, payload: &PositionLiquidatedPayload) {
        println!(
            "[{}] LIQUIDATED {} pnl=${:.2}",
            self.name,
            if payload.side == Side::Buy { "LONG" } else { "SHORT" },
            payload.pnl as f64 / 1_000_000.0
        );
        self.collateral_in_position = 0;
        self.has_position = false;
        self.position_side = None;
        self.entry_price = None;
        self.liquidations += 1;
        self.total_pnl += payload.pnl;
    }

    fn handle_market_state(&mut self, payload: &MarketStatePayload) {
        if payload.symbol == self.symbol {
            self.oi_long_usd = payload.oi_long_usd;
            self.oi_short_usd = payload.oi_short_usd;
            self.liquidity_usd = payload.liquidity_usd;
        }
    }

    fn execute_hodler(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        if let TradingStrategy::Hodler {
            side,
            hold_duration_sec,
            take_profit_pct,
            stop_loss_pct,
            ..
        } = &self.strategy
        {
            if !self.has_position {
                if self.current_price.is_some() {
                    self.open_position(sim, *side, now_ns);
                }
                return;
            }

            if let Some(pnl) = self.calculate_pnl_pct() {
                if let Some(tp) = take_profit_pct {
                    if pnl >= *tp {
                        self.close_position(sim, "TP", now_ns);
                        return;
                    }
                }
                if let Some(sl) = stop_loss_pct {
                    if pnl <= -*sl {
                        self.close_position(sim, "SL", now_ns);
                        return;
                    }
                }
            }

            let held = (now_ns - self.position_opened_at) / 1_000_000_000;
            if held >= *hold_duration_sec {
                self.close_position(sim, "TIME", now_ns);
            }
        }
    }

    fn execute_institutional(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        if let TradingStrategy::Institutional {
            side,
            take_profit_pct,
            stop_loss_pct,
            min_hold_sec,
            max_hold_sec,
            reentry_delay_sec,
            ..
        } = &self.strategy
        {
            if !self.has_position {
                let since_close = (now_ns - self.last_close_at) / 1_000_000_000;
                if self.last_close_at > 0 && since_close < *reentry_delay_sec {
                    return;
                }
                if self.current_price.is_some() {
                    self.open_position(sim, *side, now_ns);
                }
                return;
            }

            let held = (now_ns - self.position_opened_at) / 1_000_000_000;
            if held < *min_hold_sec {
                return;
            }

            if let Some(pnl) = self.calculate_pnl_pct() {
                if pnl >= *take_profit_pct {
                    self.close_position(sim, "TP", now_ns);
                    return;
                }
                if pnl <= -*stop_loss_pct {
                    self.close_position(sim, "SL", now_ns);
                    return;
                }
            }

            if held >= *max_hold_sec {
                self.close_position(sim, "MAX_TIME", now_ns);
            }
        }
    }

    fn execute_trend_follower(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        if let TradingStrategy::TrendFollower {
            lookback_sec,
            threshold_pct,
            take_profit_pct,
            stop_loss_pct,
            ..
        } = &self.strategy
        {
            if self.has_position {
                if let Some(pnl) = self.calculate_pnl_pct() {
                    if let Some(tp) = take_profit_pct {
                        if pnl >= *tp {
                            self.close_position(sim, "TP", now_ns);
                            return;
                        }
                    }
                    if let Some(sl) = stop_loss_pct {
                        if pnl <= -*sl {
                            self.close_position(sim, "SL", now_ns);
                            return;
                        }
                    }
                }
            }

            if self.price_history.len() < 2 {
                return;
            }

            let lookback_ns = *lookback_sec * 1_000_000_000;
            let cutoff = now_ns.saturating_sub(lookback_ns);
            let old_price = self.price_history.iter().find(|(ts, _)| *ts >= cutoff).map(|(_, p)| *p);

            let current = match self.current_price {
                Some(p) => p,
                None => return,
            };
            let old = match old_price {
                Some(p) => p,
                None => return,
            };

            let change = (current as f64 - old as f64) / old as f64 * 100.0;

            if !self.has_position {
                if change > *threshold_pct {
                    self.open_position(sim, Side::Buy, now_ns);
                } else if change < -*threshold_pct {
                    self.open_position(sim, Side::Sell, now_ns);
                }
            } else if let Some(side) = self.position_side {
                let should_close = match side {
                    Side::Buy => change < -*threshold_pct / 2.0,
                    Side::Sell => change > *threshold_pct / 2.0,
                };
                if should_close {
                    self.close_position(sim, "REVERSAL", now_ns);
                }
            }
        }
    }

    fn execute_mean_reversion(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        if let TradingStrategy::MeanReversion {
            lookback_periods,
            entry_deviation_pct,
            exit_deviation_pct,
            max_hold_sec,
            ..
        } = &self.strategy
        {
            let current = match self.current_price {
                Some(p) => p as f64,
                None => return,
            };
            let sma = match self.calculate_sma(*lookback_periods) {
                Some(s) => s,
                None => return,
            };

            let deviation = (current - sma) / sma * 100.0;

            if self.has_position {
                let held = (now_ns - self.position_opened_at) / 1_000_000_000;

                let should_exit = match self.position_side {
                    Some(Side::Buy) => deviation >= -*exit_deviation_pct,
                    Some(Side::Sell) => deviation <= *exit_deviation_pct,
                    None => false,
                };

                if should_exit {
                    self.close_position(sim, "MEAN_REV", now_ns);
                } else if held >= *max_hold_sec {
                    self.close_position(sim, "MAX_TIME", now_ns);
                }
            } else {
                if deviation <= -*entry_deviation_pct {
                    self.open_position(sim, Side::Buy, now_ns);
                } else if deviation >= *entry_deviation_pct {
                    self.open_position(sim, Side::Sell, now_ns);
                }
            }
        }
    }

    /// Arbitrageur: opens against OI imbalance to get price impact bonus
    fn execute_arbitrageur(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        if let TradingStrategy::Arbitrageur {
            min_imbalance_pct,
            hold_duration_sec,
            take_profit_pct,
            stop_loss_pct,
            ..
        } = &self.strategy
        {
            let imbalance = self.calculate_oi_imbalance_pct();

            if self.has_position {
                // Check TP/SL
                if let Some(pnl) = self.calculate_pnl_pct() {
                    if let Some(tp) = take_profit_pct {
                        if pnl >= *tp {
                            self.close_position(sim, "TP", now_ns);
                            return;
                        }
                    }
                    if let Some(sl) = stop_loss_pct {
                        if pnl <= -*sl {
                            self.close_position(sim, "SL", now_ns);
                            return;
                        }
                    }
                }

                // Check hold duration
                let held = (now_ns - self.position_opened_at) / 1_000_000_000;
                if held >= *hold_duration_sec {
                    self.close_position(sim, "TIME", now_ns);
                    return;
                }

                // Check if imbalance flipped - close early
                if let Some(side) = self.position_side {
                    let should_close = match side {
                        Side::Buy => imbalance < -*min_imbalance_pct,
                        Side::Sell => imbalance > *min_imbalance_pct,
                    };
                    if should_close {
                        self.close_position(sim, "REBALANCED", now_ns);
                    }
                }
            } else {
                if self.current_price.is_none() {
                    return;
                }

                // Long-heavy → open SHORT, Short-heavy → open LONG
                if imbalance > *min_imbalance_pct {
                    println!("[{}] ARB: Long-heavy {:.1}%, opening SHORT", self.name, imbalance);
                    self.open_position(sim, Side::Sell, now_ns);
                } else if imbalance < -*min_imbalance_pct {
                    println!("[{}] ARB: Short-heavy {:.1}%, opening LONG", self.name, imbalance);
                    self.open_position(sim, Side::Buy, now_ns);
                }
            }
        }
    }

    /// FundingHarvester: sits on smaller side to receive funding payments
    fn execute_funding_harvester(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        if let TradingStrategy::FundingHarvester {
            min_imbalance_pct,
            min_hold_sec,
            max_hold_sec,
            exit_imbalance_pct,
            stop_loss_pct,
            ..
        } = &self.strategy
        {
            let imbalance = self.calculate_oi_imbalance_pct().abs();

            if self.has_position {
                let held = (now_ns - self.position_opened_at) / 1_000_000_000;

                if let Some(pnl) = self.calculate_pnl_pct() {
                    if pnl <= -*stop_loss_pct {
                        self.close_position(sim, "SL", now_ns);
                        return;
                    }
                }

                if held >= *max_hold_sec {
                    self.close_position(sim, "MAX_TIME", now_ns);
                    return;
                }

                if held < *min_hold_sec {
                    return;
                }

                if let Some(our_side) = self.position_side {
                    let smaller_side = self.get_smaller_oi_side();
                    if let Some(smaller) = smaller_side {
                        if our_side != smaller && imbalance > *exit_imbalance_pct {
                            self.close_position(sim, "WRONG_SIDE", now_ns);
                            return;
                        }
                    }
                }

                if imbalance < *min_imbalance_pct / 2.0 {
                    self.close_position(sim, "LOW_IMBALANCE", now_ns);
                }
            } else {
                if self.current_price.is_none() {
                    return;
                }

                if imbalance < *min_imbalance_pct {
                    return;
                }

                if let Some(side) = self.get_smaller_oi_side() {
                    println!(
                        "[{}] FUNDING: Imbalance {:.1}%, joining {} side",
                        self.name,
                        self.calculate_oi_imbalance_pct(),
                        if side == Side::Buy { "LONG" } else { "SHORT" }
                    );
                    self.open_position(sim, side, now_ns);
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
        let strategy = match &self.strategy {
            TradingStrategy::Hodler { side, leverage, .. } => format!("Hodler({:?},{}x)", side, leverage),
            TradingStrategy::Institutional { side, leverage, .. } => format!("Inst({:?},{}x)", side, leverage),
            TradingStrategy::TrendFollower { leverage, .. } => format!("Trend({}x)", leverage),
            TradingStrategy::MeanReversion { leverage, .. } => format!("MeanRev({}x)", leverage),
            TradingStrategy::Arbitrageur { leverage, .. } => format!("Arb({}x)", leverage),
            TradingStrategy::FundingHarvester { leverage, .. } => format!("FundHarv({}x)", leverage),
        };

        // NEW: Apply staggered start
        let delay_ms = self.start_delay_ns / 1_000_000;
        if delay_ms > 0 {
            println!(
                "[{}] START {} bal=${:.0}{} (delayed {}ms)",
                self.name,
                strategy,
                self.balance as f64 / 1_000_000.0,
                self.address
                    .as_deref()
                    .map(|addr| format!(" addr={}", addr))
                    .unwrap_or_default(),
                delay_ms
            );
        } else {
            println!(
                "[{}] START {} bal=${:.0}{}",
                self.name,
                strategy,
                self.balance as f64 / 1_000_000.0,
                self.address
                    .as_deref()
                    .map(|addr| format!(" addr={}", addr))
                    .unwrap_or_default()
            );
        }

        sim.wakeup(self.id, sim.now_ns() + self.start_delay_ns + self.wake_interval_ns);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        match &self.strategy {
            TradingStrategy::Hodler { .. } => self.execute_hodler(sim, now_ns),
            TradingStrategy::Institutional { .. } => self.execute_institutional(sim, now_ns),
            TradingStrategy::TrendFollower { .. } => self.execute_trend_follower(sim, now_ns),
            TradingStrategy::MeanReversion { .. } => self.execute_mean_reversion(sim, now_ns),
            TradingStrategy::Arbitrageur { .. } => self.execute_arbitrageur(sim, now_ns),
            TradingStrategy::FundingHarvester { .. } => self.execute_funding_harvester(sim, now_ns),
        }
        sim.wakeup(self.id, now_ns + self.wake_interval_ns);
    }

    fn on_message(&mut self, sim: &mut dyn SimulatorApi, msg: &Message) {
        match msg.msg_type {
            MessageType::OracleTick => {
                if let MessagePayload::OracleTick(OracleTickPayload { symbol, price, .. }) = &msg.payload {
                    if *symbol == self.symbol {
                        let mid = (price.min + price.max) / 2;
                        self.current_price = Some(mid);
                        self.price_history.push_back((msg.at, mid));
                        if self.price_history.len() > 200 {
                            self.price_history.pop_front();
                        }
                    }
                }
            }
            MessageType::MarketState => {
                if let MessagePayload::MarketState(p) = &msg.payload {
                    self.handle_market_state(p);
                }
            }
            MessageType::PositionLiquidated => {
                if let MessagePayload::PositionLiquidated(p) = &msg.payload {
                    if p.symbol == self.symbol {
                        self.handle_liquidation(p);
                    }
                }
            }
            MessageType::OrderExecuted => {
                if let MessagePayload::OrderExecuted(p) = &msg.payload {
                    if p.symbol == self.symbol {
                        self.handle_order_executed(sim, p);
                    }
                }
            }
            MessageType::OrderPending => {
                if let MessagePayload::Text(text) = &msg.payload {
                    if let Some(id_str) = text.strip_prefix("order_id:") {
                        if let Ok(id) = id_str.parse::<u64>() {
                            if self.pending_sl_order_id.is_none() {
                                self.pending_sl_order_id = Some(id);
                            } else if self.pending_tp_order_id.is_none() {
                                self.pending_tp_order_id = Some(id);
                            }
                        }
                    }
                }
            }
            MessageType::OrderTriggered | MessageType::OrderCancelled => {
                // SL/TP was triggered or cancelled - position state already handled
            }
            _ => {}
        }
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        let pnl = if self.total_pnl >= 0 {
            format!("+${:.2}", self.total_pnl as f64 / 1_000_000.0)
        } else {
            format!("-${:.2}", (-self.total_pnl) as f64 / 1_000_000.0)
        };
        println!(
            "[{}] STOP: open={} close={} liq={} skip={} pnl={} bal=${:.0}",
            self.name,
            self.trades_opened,
            self.trades_closed,
            self.liquidations,
            self.skipped_due_to_oi,
            pnl,
            self.balance as f64 / 1_000_000.0
        );
    }
}
