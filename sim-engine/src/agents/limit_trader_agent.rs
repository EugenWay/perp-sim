use crate::agents::Agent;
use crate::messages::{
    AgentId, CancelOrderPayload, ExecutionType, Message, MessagePayload, MessageType,
    OrderPayload, OrderType, OracleTickPayload, OrderExecutedPayload, OrderExecutionType,
    Side, SimulatorApi,
};
use std::collections::VecDeque;

const DEFAULT_BALANCE: i128 = 50_000_000_000;
const MAX_PRICE_HISTORY: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderMode {
    Passive,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Buy,
    Sell,
    None,
}

#[derive(Debug, Clone)]
pub enum LimitStrategy {
    MeanReversion {
        entry_offset_pct: f64,
        stop_loss_pct: f64,
        take_profit_pct: f64,
        leverage: u32,
        trend_lookback: u32,
    },
    Breakout {
        breakout_offset_pct: f64,
        stop_loss_pct: f64,
        take_profit_pct: f64,
        leverage: u32,
        direction: Side,
    },
    Grid {
        levels: u32,
        spacing_pct: f64,
        qty_per_level: f64,
        leverage: u32,
        take_profit_pct: f64,
    },
    Smart {
        sma_fast: u32,
        sma_slow: u32,
        rsi_period: u32,
        rsi_low: f64,
        rsi_high: f64,
        atr_period: u32,
        entry_atr_mult: f64,
        stop_atr_mult: f64,
        take_atr_mult: f64,
        leverage: u32,
        order_mode: OrderMode,
    },
}

#[derive(Debug, Clone)]
pub struct LimitTraderConfig {
    pub name: String,
    pub exchange_id: AgentId,
    pub symbol: String,
    pub address: Option<String>,
    pub strategy: LimitStrategy,
    pub qty: f64,
    pub wake_interval_ms: u64,
    pub balance: Option<i128>,
}

#[derive(Debug, Clone, Copy)]
struct Candle {
    #[allow(dead_code)]
    open: u64,
    high: u64,
    low: u64,
    close: u64,
}

pub struct LimitTraderAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    symbol: String,
    address: Option<String>,
    strategy: LimitStrategy,
    qty: f64,
    wake_interval_ns: u64,

    balance: i128,

    has_position: bool,
    position_side: Option<Side>,
    entry_price: Option<u64>,

    pending_entry_order: Option<u64>,
    pending_entry_side: Option<Side>,
    pending_sl_order: Option<u64>,
    pending_tp_order: Option<u64>,

    price_history: VecDeque<u64>,
    candles: VecDeque<Candle>,
    current_candle: Option<Candle>,
    last_candle_time: u64,
    candle_duration_ns: u64,
    current_price: Option<u64>,

    last_signal: Signal,
    last_atr: Option<f64>,

    orders_submitted: u32,
    orders_filled: u32,
    orders_cancelled: u32,
    total_pnl: i128,
}

impl LimitTraderAgent {
    pub fn new(id: AgentId, config: LimitTraderConfig) -> Self {
        Self {
            id,
            name: config.name,
            exchange_id: config.exchange_id,
            symbol: config.symbol,
            address: config.address,
            strategy: config.strategy,
            qty: config.qty,
            wake_interval_ns: config.wake_interval_ms * 1_000_000,
            balance: config.balance.unwrap_or(DEFAULT_BALANCE),
            has_position: false,
            position_side: None,
            entry_price: None,
            pending_entry_order: None,
            pending_entry_side: None,
            pending_sl_order: None,
            pending_tp_order: None,
            price_history: VecDeque::with_capacity(MAX_PRICE_HISTORY),
            candles: VecDeque::with_capacity(100),
            current_candle: None,
            last_candle_time: 0,
            candle_duration_ns: 5_000_000_000, // 5 sec candles
            current_price: None,
            last_signal: Signal::None,
            last_atr: None,
            orders_submitted: 0,
            orders_filled: 0,
            orders_cancelled: 0,
            total_pnl: 0,
        }
    }

    pub fn set_address(&mut self, address: String) {
        self.address = Some(address);
    }

    // ========== INDICATORS ==========

    fn calc_sma(&self, period: u32) -> Option<f64> {
        if self.price_history.len() < period as usize {
            return None;
        }
        let sum: u64 = self.price_history.iter().rev().take(period as usize).sum();
        Some(sum as f64 / period as f64)
    }

    #[allow(dead_code)]
    fn calc_ema(&self, period: u32) -> Option<f64> {
        if self.price_history.len() < period as usize {
            return None;
        }
        let k = 2.0 / (period as f64 + 1.0);
        let prices: Vec<u64> = self.price_history.iter().rev().take(period as usize * 2).copied().collect();
        
        let mut ema = prices.last().copied()? as f64;
        for &p in prices.iter().rev().skip(1) {
            ema = (p as f64) * k + ema * (1.0 - k);
        }
        Some(ema)
    }

    fn calc_rsi(&self, period: u32) -> Option<f64> {
        if self.price_history.len() < (period + 1) as usize {
            return None;
        }

        let prices: Vec<u64> = self.price_history.iter().rev().take((period + 1) as usize).copied().collect();
        
        let mut gains = 0.0;
        let mut losses = 0.0;

        for i in 0..period as usize {
            let diff = prices[i] as f64 - prices[i + 1] as f64;
            if diff > 0.0 {
                gains += diff;
            } else {
                losses += -diff;
            }
        }

        let avg_gain = gains / period as f64;
        let avg_loss = losses / period as f64;

        if avg_loss < 0.0001 {
            return Some(100.0);
        }

        let rs = avg_gain / avg_loss;
        Some(100.0 - (100.0 / (1.0 + rs)))
    }

    fn calc_atr(&self, period: u32) -> Option<f64> {
        if self.candles.len() < period as usize {
            return None;
        }

        let candles: Vec<&Candle> = self.candles.iter().rev().take(period as usize).collect();
        
        let mut tr_sum = 0.0;
        for (i, c) in candles.iter().enumerate() {
            let high_low = (c.high - c.low) as f64;
            let tr = if i + 1 < candles.len() {
                let prev_close = candles[i + 1].close as f64;
                let high_close = (c.high as f64 - prev_close).abs();
                let low_close = (c.low as f64 - prev_close).abs();
                high_low.max(high_close).max(low_close)
            } else {
                high_low
            };
            tr_sum += tr;
        }

        Some(tr_sum / period as f64)
    }

    fn update_candle(&mut self, price: u64, now_ns: u64) {
        if self.last_candle_time == 0 {
            self.last_candle_time = now_ns;
            self.current_candle = Some(Candle {
                open: price,
                high: price,
                low: price,
                close: price,
            });
            return;
        }

        if now_ns - self.last_candle_time >= self.candle_duration_ns {
            if let Some(candle) = self.current_candle.take() {
                self.candles.push_back(candle);
                if self.candles.len() > 100 {
                    self.candles.pop_front();
                }
            }
            self.last_candle_time = now_ns;
            self.current_candle = Some(Candle {
                open: price,
                high: price,
                low: price,
                close: price,
            });
        } else if let Some(ref mut candle) = self.current_candle {
            candle.high = candle.high.max(price);
            candle.low = candle.low.min(price);
            candle.close = price;
        }
    }

    // ========== SIGNAL LOGIC ==========

    fn detect_trend(&self, lookback: u32) -> Option<bool> {
        if self.price_history.len() < lookback as usize {
            return None;
        }

        let recent: Vec<u64> = self.price_history
            .iter()
            .rev()
            .take(lookback as usize)
            .copied()
            .collect();

        let first = *recent.last()?;
        let last = *recent.first()?;

        Some(last > first)
    }

    fn calc_smart_signal(&mut self) -> Signal {
        if let LimitStrategy::Smart {
            sma_fast,
            sma_slow,
            rsi_period,
            rsi_low,
            rsi_high,
            atr_period,
            ..
        } = &self.strategy
        {
            let sma_f = match self.calc_sma(*sma_fast) {
                Some(v) => v,
                None => return Signal::None,
            };
            let sma_s = match self.calc_sma(*sma_slow) {
                Some(v) => v,
                None => return Signal::None,
            };
            let rsi = match self.calc_rsi(*rsi_period) {
                Some(v) => v,
                None => return Signal::None,
            };

            self.last_atr = self.calc_atr(*atr_period);

            if sma_f > sma_s && rsi < *rsi_low {
                return Signal::Buy;
            }
            if sma_f < sma_s && rsi > *rsi_high {
                return Signal::Sell;
            }
        }
        Signal::None
    }

    // ========== ORDER MANAGEMENT ==========

    fn submit_entry_order(&mut self, sim: &mut dyn SimulatorApi, side: Side, trigger_price: u64) {
        let leverage = self.get_leverage();

        let order = OrderPayload {
            symbol: self.symbol.clone(),
            side,
            order_type: OrderType::Increase,
            execution_type: ExecutionType::Limit,
            qty: Some(self.qty),
            leverage: Some(leverage),
            size_delta_usd: None,
            trigger_price: Some(trigger_price),
            acceptable_price: None,
            valid_for_sec: Some(3600),
        };

        println!(
            "[{}] SUBMIT LIMIT {} @ ${:.2}",
            self.name,
            if side == Side::Buy { "BUY" } else { "SELL" },
            trigger_price as f64 / 1_000_000.0
        );

        sim.send(
            self.id,
            self.exchange_id,
            MessageType::SubmitOrder,
            MessagePayload::Order(order),
        );

        self.pending_entry_side = Some(side);
        self.orders_submitted += 1;
    }

    fn cancel_pending_entry(&mut self, sim: &mut dyn SimulatorApi) {
        if let Some(order_id) = self.pending_entry_order.take() {
            println!("[{}] CANCEL #{}", self.name, order_id);
            sim.send(
                self.id,
                self.exchange_id,
                MessageType::CancelOrder,
                MessagePayload::CancelOrder(CancelOrderPayload { order_id }),
            );
            self.pending_entry_side = None;
        }
    }

    fn submit_sl_tp_orders(&mut self, sim: &mut dyn SimulatorApi, entry_price: u64) {
        let side = match self.position_side {
            Some(s) => s,
            None => return,
        };

        let (sl_price, tp_price) = self.calc_sl_tp_prices(entry_price, side);

        // Stop Loss
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

        println!("[{}] SUBMIT SL @ ${:.2}", self.name, sl_price as f64 / 1_000_000.0);

        sim.send(
            self.id,
            self.exchange_id,
            MessageType::SubmitOrder,
            MessagePayload::Order(sl_order),
        );

        // Take Profit
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

        println!("[{}] SUBMIT TP @ ${:.2}", self.name, tp_price as f64 / 1_000_000.0);

        sim.send(
            self.id,
            self.exchange_id,
            MessageType::SubmitOrder,
            MessagePayload::Order(tp_order),
        );

        self.orders_submitted += 2;
    }

    fn calc_sl_tp_prices(&self, entry_price: u64, side: Side) -> (u64, u64) {
        match &self.strategy {
            LimitStrategy::Smart {
                stop_atr_mult,
                take_atr_mult,
                ..
            } => {
                let atr = self.last_atr.unwrap_or(entry_price as f64 * 0.02);
                let (sl, tp) = match side {
                    Side::Buy => (
                        (entry_price as f64 - atr * stop_atr_mult) as u64,
                        (entry_price as f64 + atr * take_atr_mult) as u64,
                    ),
                    Side::Sell => (
                        (entry_price as f64 + atr * stop_atr_mult) as u64,
                        (entry_price as f64 - atr * take_atr_mult) as u64,
                    ),
                };
                (sl, tp)
            }
            _ => {
                let (sl_pct, tp_pct) = self.get_sl_tp_pct();
                let (sl, tp) = match side {
                    Side::Buy => (
                        ((entry_price as f64) * (1.0 - sl_pct / 100.0)) as u64,
                        ((entry_price as f64) * (1.0 + tp_pct / 100.0)) as u64,
                    ),
                    Side::Sell => (
                        ((entry_price as f64) * (1.0 + sl_pct / 100.0)) as u64,
                        ((entry_price as f64) * (1.0 - tp_pct / 100.0)) as u64,
                    ),
                };
                (sl, tp)
            }
        }
    }

    fn get_leverage(&self) -> u32 {
        match &self.strategy {
            LimitStrategy::MeanReversion { leverage, .. } => *leverage,
            LimitStrategy::Breakout { leverage, .. } => *leverage,
            LimitStrategy::Grid { leverage, .. } => *leverage,
            LimitStrategy::Smart { leverage, .. } => *leverage,
        }
    }

    fn get_sl_tp_pct(&self) -> (f64, f64) {
        match &self.strategy {
            LimitStrategy::MeanReversion { stop_loss_pct, take_profit_pct, .. } => (*stop_loss_pct, *take_profit_pct),
            LimitStrategy::Breakout { stop_loss_pct, take_profit_pct, .. } => (*stop_loss_pct, *take_profit_pct),
            LimitStrategy::Grid { take_profit_pct, .. } => (5.0, *take_profit_pct),
            LimitStrategy::Smart { .. } => (3.0, 2.0), // fallback
        }
    }

    fn get_order_mode(&self) -> OrderMode {
        match &self.strategy {
            LimitStrategy::Smart { order_mode, .. } => *order_mode,
            _ => OrderMode::Passive,
        }
    }

    // ========== STRATEGY EXECUTION ==========

    fn execute_mean_reversion(&mut self, sim: &mut dyn SimulatorApi) {
        if let LimitStrategy::MeanReversion { entry_offset_pct, trend_lookback, .. } = &self.strategy {
            if self.has_position || self.pending_entry_order.is_some() {
                return;
            }

            let current_price = match self.current_price {
                Some(p) => p,
                None => return,
            };

            let trend_up = match self.detect_trend(*trend_lookback) {
                Some(t) => t,
                None => return,
            };

            let (side, trigger_price) = if trend_up {
                let price = ((current_price as f64) * (1.0 - entry_offset_pct / 100.0)) as u64;
                (Side::Buy, price)
            } else {
                let price = ((current_price as f64) * (1.0 + entry_offset_pct / 100.0)) as u64;
                (Side::Sell, price)
            };

            self.submit_entry_order(sim, side, trigger_price);
        }
    }

    fn execute_breakout(&mut self, sim: &mut dyn SimulatorApi) {
        if let LimitStrategy::Breakout { breakout_offset_pct, direction, .. } = &self.strategy {
            if self.has_position || self.pending_entry_order.is_some() {
                return;
            }

            let current_price = match self.current_price {
                Some(p) => p,
                None => return,
            };

            let trigger_price = match direction {
                Side::Buy => ((current_price as f64) * (1.0 + breakout_offset_pct / 100.0)) as u64,
                Side::Sell => ((current_price as f64) * (1.0 - breakout_offset_pct / 100.0)) as u64,
            };

            self.submit_entry_order(sim, *direction, trigger_price);
        }
    }

    fn execute_smart(&mut self, sim: &mut dyn SimulatorApi) {
        if self.has_position {
            return;
        }

        let signal = self.calc_smart_signal();
        let order_mode = self.get_order_mode();

        // Active mode: cancel if signal changed
        if order_mode == OrderMode::Active && self.pending_entry_order.is_some() {
            let signal_matches = match (&signal, &self.pending_entry_side) {
                (Signal::Buy, Some(Side::Buy)) => true,
                (Signal::Sell, Some(Side::Sell)) => true,
                (Signal::None, _) => false,
                _ => false,
            };
            if !signal_matches {
                self.cancel_pending_entry(sim);
            }
        }

        if self.pending_entry_order.is_some() {
            return;
        }

        if signal == Signal::None {
            return;
        }

        let current_price = match self.current_price {
            Some(p) => p,
            None => return,
        };

        let atr = self.last_atr.unwrap_or(current_price as f64 * 0.01);

        if let LimitStrategy::Smart { entry_atr_mult, .. } = &self.strategy {
            let (side, trigger_price) = match signal {
                Signal::Buy => {
                    let price = (current_price as f64 - atr * entry_atr_mult) as u64;
                    (Side::Buy, price)
                }
                Signal::Sell => {
                    let price = (current_price as f64 + atr * entry_atr_mult) as u64;
                    (Side::Sell, price)
                }
                Signal::None => return,
            };

            self.last_signal = signal;
            self.submit_entry_order(sim, side, trigger_price);
        }
    }

    fn handle_order_executed(&mut self, sim: &mut dyn SimulatorApi, payload: &OrderExecutedPayload) {
        match payload.order_type {
            OrderExecutionType::Increase => {
                self.has_position = true;
                self.position_side = Some(payload.side);
                self.pending_entry_order = None;
                self.pending_entry_side = None;
                self.orders_filled += 1;

                if let Some(price) = self.current_price {
                    self.entry_price = Some(price);
                    self.submit_sl_tp_orders(sim, price);
                }

                println!("[{}] ENTRY FILLED {:?}", self.name, payload.side);
            }
            OrderExecutionType::Decrease => {
                self.has_position = false;
                self.position_side = None;
                self.entry_price = None;
                self.pending_sl_order = None;
                self.pending_tp_order = None;
                self.orders_filled += 1;
                self.total_pnl += payload.pnl;
                self.last_signal = Signal::None;

                println!(
                    "[{}] CLOSED pnl=${:.2}",
                    self.name,
                    payload.pnl as f64 / 1_000_000.0
                );
            }
            OrderExecutionType::Liquidation => {
                self.has_position = false;
                self.position_side = None;
                self.total_pnl += payload.pnl;
                self.last_signal = Signal::None;
            }
        }
    }
}

impl Agent for LimitTraderAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn on_start(&mut self, sim: &mut dyn SimulatorApi) {
        let strategy_name = match &self.strategy {
            LimitStrategy::MeanReversion { .. } => "MeanReversion".to_string(),
            LimitStrategy::Breakout { direction, .. } => {
                if *direction == Side::Buy { "Breakout(UP)".to_string() } else { "Breakout(DOWN)".to_string() }
            }
            LimitStrategy::Grid { .. } => "Grid".to_string(),
            LimitStrategy::Smart { order_mode, .. } => {
                format!("Smart({:?})", order_mode)
            }
        };

        println!(
            "[{}] START {} bal=${:.0}{}",
            self.name,
            strategy_name,
            self.balance as f64 / 1_000_000.0,
            self.address
                .as_deref()
                .map(|addr| format!(" addr={}", addr))
                .unwrap_or_default()
        );

        sim.wakeup(self.id, sim.now_ns() + self.wake_interval_ns);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        match &self.strategy {
            LimitStrategy::MeanReversion { .. } => self.execute_mean_reversion(sim),
            LimitStrategy::Breakout { .. } => self.execute_breakout(sim),
            LimitStrategy::Grid { .. } => {}
            LimitStrategy::Smart { .. } => self.execute_smart(sim),
        }

        sim.wakeup(self.id, now_ns + self.wake_interval_ns);
    }

    fn on_message(&mut self, sim: &mut dyn SimulatorApi, msg: &Message) {
        let now_ns = sim.now_ns();

        match msg.msg_type {
            MessageType::OracleTick => {
                if let MessagePayload::OracleTick(OracleTickPayload { symbol, price, .. }) = &msg.payload {
                    if *symbol == self.symbol {
                        let mid = (price.min + price.max) / 2;
                        self.current_price = Some(mid);
                        self.price_history.push_back(mid);
                        if self.price_history.len() > MAX_PRICE_HISTORY {
                            self.price_history.pop_front();
                        }
                        self.update_candle(mid, now_ns);
                    }
                }
            }
            MessageType::OrderPending => {
                if let MessagePayload::Text(text) = &msg.payload {
                    if let Some(id_str) = text.strip_prefix("order_id:") {
                        if let Ok(id) = id_str.parse::<u64>() {
                            if !self.has_position {
                                self.pending_entry_order = Some(id);
                            }
                        }
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
            MessageType::PositionLiquidated => {
                if let MessagePayload::PositionLiquidated(p) = &msg.payload {
                    if p.symbol == self.symbol {
                        self.has_position = false;
                        self.position_side = None;
                        self.total_pnl += p.pnl;
                        self.last_signal = Signal::None;
                    }
                }
            }
            MessageType::OrderRejected => {
                // On-chain tx failed — clear pending state so we can retry
                if self.pending_entry_order.is_some() {
                    eprintln!("[{}] OrderRejected — clearing pending entry", self.name);
                    self.pending_entry_order = None;
                    self.pending_entry_side = None;
                }
            }
            MessageType::OrderCancelled => {
                if let MessagePayload::Text(text) = &msg.payload {
                    if text.contains("order_id:") {
                        if self.pending_entry_order.is_some() {
                            self.pending_entry_order = None;
                            self.pending_entry_side = None;
                        }
                    }
                }
                self.orders_cancelled += 1;
            }
            _ => {}
        }
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        let pnl_str = if self.total_pnl >= 0 {
            format!("+${:.2}", self.total_pnl as f64 / 1_000_000.0)
        } else {
            format!("-${:.2}", (-self.total_pnl) as f64 / 1_000_000.0)
        };

        println!(
            "[{}] STOP: submitted={} filled={} cancelled={} pnl={}",
            self.name,
            self.orders_submitted,
            self.orders_filled,
            self.orders_cancelled,
            pnl_str
        );
    }
}
