use crate::agents::Agent;
use crate::messages::{
    AgentId, ExecutionType, Message, MessagePayload, MessageType, OrderPayload, OrderType,
    OracleTickPayload, OrderExecutedPayload, OrderExecutionType, Side, SimulatorApi,
};
use std::collections::VecDeque;

const DEFAULT_BALANCE: i128 = 50_000_000_000;

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
}

#[derive(Debug, Clone)]
pub struct LimitTraderConfig {
    pub name: String,
    pub exchange_id: AgentId,
    pub symbol: String,
    pub strategy: LimitStrategy,
    pub qty: f64,
    pub wake_interval_ms: u64,
    pub balance: Option<i128>,
}

pub struct LimitTraderAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    symbol: String,
    strategy: LimitStrategy,
    qty: f64,
    wake_interval_ns: u64,

    balance: i128,
    collateral_in_position: i128,

    has_position: bool,
    position_side: Option<Side>,
    entry_price: Option<u64>,

    pending_entry_order: Option<u64>,
    pending_sl_order: Option<u64>,
    pending_tp_order: Option<u64>,

    price_history: VecDeque<u64>,
    current_price: Option<u64>,

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
            strategy: config.strategy,
            qty: config.qty,
            wake_interval_ns: config.wake_interval_ms * 1_000_000,
            balance: config.balance.unwrap_or(DEFAULT_BALANCE),
            collateral_in_position: 0,
            has_position: false,
            position_side: None,
            entry_price: None,
            pending_entry_order: None,
            pending_sl_order: None,
            pending_tp_order: None,
            price_history: VecDeque::with_capacity(100),
            current_price: None,
            orders_submitted: 0,
            orders_filled: 0,
            orders_cancelled: 0,
            total_pnl: 0,
        }
    }

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

    fn submit_entry_order(&mut self, sim: &mut dyn SimulatorApi, side: Side, trigger_price: u64, _now_ns: u64) {
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

        self.orders_submitted += 1;
    }

    fn submit_sl_tp_orders(&mut self, sim: &mut dyn SimulatorApi, entry_price: u64) {
        let (sl_pct, tp_pct) = self.get_sl_tp_pct();
        let side = match self.position_side {
            Some(s) => s,
            None => return,
        };

        // Stop Loss
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

        println!("[{}] SUBMIT SL @ ${:.2}", self.name, sl_price as f64 / 1_000_000.0);

        sim.send(
            self.id,
            self.exchange_id,
            MessageType::SubmitOrder,
            MessagePayload::Order(sl_order),
        );

        // Take Profit
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

        println!("[{}] SUBMIT TP @ ${:.2}", self.name, tp_price as f64 / 1_000_000.0);

        sim.send(
            self.id,
            self.exchange_id,
            MessageType::SubmitOrder,
            MessagePayload::Order(tp_order),
        );

        self.orders_submitted += 2;
    }

    fn get_leverage(&self) -> u32 {
        match &self.strategy {
            LimitStrategy::MeanReversion { leverage, .. } => *leverage,
            LimitStrategy::Breakout { leverage, .. } => *leverage,
            LimitStrategy::Grid { leverage, .. } => *leverage,
        }
    }

    fn get_sl_tp_pct(&self) -> (f64, f64) {
        match &self.strategy {
            LimitStrategy::MeanReversion { stop_loss_pct, take_profit_pct, .. } => (*stop_loss_pct, *take_profit_pct),
            LimitStrategy::Breakout { stop_loss_pct, take_profit_pct, .. } => (*stop_loss_pct, *take_profit_pct),
            LimitStrategy::Grid { take_profit_pct, .. } => (5.0, *take_profit_pct),
        }
    }

    fn execute_mean_reversion(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
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

            self.submit_entry_order(sim, side, trigger_price, now_ns);
        }
    }

    fn execute_breakout(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
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

            self.submit_entry_order(sim, *direction, trigger_price, now_ns);
        }
    }

    fn handle_order_executed(&mut self, sim: &mut dyn SimulatorApi, payload: &OrderExecutedPayload) {
        match payload.order_type {
            OrderExecutionType::Increase => {
                self.has_position = true;
                self.position_side = Some(payload.side);
                self.pending_entry_order = None;
                self.orders_filled += 1;

                if let Some(price) = self.current_price {
                    self.entry_price = Some(price);
                    self.submit_sl_tp_orders(sim, price);
                }

                println!("[{}] ENTRY FILLED", self.name);
            }
            OrderExecutionType::Decrease => {
                self.has_position = false;
                self.position_side = None;
                self.entry_price = None;
                self.pending_sl_order = None;
                self.pending_tp_order = None;
                self.orders_filled += 1;
                self.total_pnl += payload.pnl;

                println!(
                    "[{}] POSITION CLOSED pnl=${:.2}",
                    self.name,
                    payload.pnl as f64 / 1_000_000.0
                );
            }
            OrderExecutionType::Liquidation => {
                self.has_position = false;
                self.position_side = None;
                self.total_pnl += payload.pnl;
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
            LimitStrategy::MeanReversion { .. } => "MeanReversion",
            LimitStrategy::Breakout { direction, .. } => {
                if *direction == Side::Buy { "Breakout(UP)" } else { "Breakout(DOWN)" }
            }
            LimitStrategy::Grid { .. } => "Grid",
        };

        println!(
            "[{}] START {} bal=${:.0}",
            self.name,
            strategy_name,
            self.balance as f64 / 1_000_000.0
        );

        sim.wakeup(self.id, sim.now_ns() + self.wake_interval_ns);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        match &self.strategy {
            LimitStrategy::MeanReversion { .. } => self.execute_mean_reversion(sim, now_ns),
            LimitStrategy::Breakout { .. } => self.execute_breakout(sim, now_ns),
            LimitStrategy::Grid { .. } => {}
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
                        self.price_history.push_back(mid);
                        if self.price_history.len() > 100 {
                            self.price_history.pop_front();
                        }
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
                    }
                }
            }
            MessageType::OrderCancelled => {
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
