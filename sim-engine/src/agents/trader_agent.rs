use crate::agents::Agent;
use crate::messages::{
    AgentId, CloseOrderPayload, MarketOrderPayload, Message, MessagePayload, MessageType, PositionLiquidatedPayload,
    Side, SimulatorApi,
};

pub struct TraderAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    symbol: String,
    wake_interval_ns: u64,
    base_qty: u64,
    // State tracking
    has_long_position: bool,
    has_short_position: bool,
    trade_count: u32,
}

impl TraderAgent {
    pub fn new(id: AgentId, name: String, exchange_id: AgentId, symbol: String) -> Self {
        Self::with_qty(id, name, exchange_id, symbol, 1)
    }

    pub fn with_qty(id: AgentId, name: String, exchange_id: AgentId, symbol: String, base_qty: u64) -> Self {
        Self {
            id,
            name,
            exchange_id,
            symbol,
            wake_interval_ns: 2_000_000_000, // 2 seconds
            base_qty: base_qty.max(1),
            has_long_position: false,
            has_short_position: false,
            trade_count: 0,
        }
    }
}

impl Agent for TraderAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn on_start(&mut self, sim: &mut dyn SimulatorApi) {
        println!(
            "[Trader {}] starting -> exchange={}, symbol={}",
            self.name, self.exchange_id, self.symbol
        );
        let now = sim.now_ns();
        sim.wakeup(self.id, now);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        self.trade_count += 1;

        // Simple pattern: open Long -> close Long -> open Short -> close Short -> repeat
        let action = self.trade_count % 4;

        // Vary qty: base_qty +/- variation based on trade count
        let variation = (self.trade_count % 3) as u64; // 0, 1, or 2
        let qty = self.base_qty.saturating_add(variation).max(1);

        match action {
            1 => {
                // Open Long
                let payload = MessagePayload::MarketOrder(MarketOrderPayload {
                    symbol: self.symbol.clone(),
                    side: Side::Buy,
                    qty,
                    leverage: 5, // default 5x leverage
                });
                println!("[Trader {}] OPEN LONG 5x qty={}", self.name, qty);
                sim.send(self.id, self.exchange_id, MessageType::MarketOrder, payload);
                self.has_long_position = true;
            }
            2 => {
                // Close Long
                if self.has_long_position {
                    let payload = MessagePayload::CloseOrder(CloseOrderPayload {
                        symbol: self.symbol.clone(),
                        side: Side::Buy,
                    });
                    println!("[Trader {}] CLOSE LONG", self.name);
                    sim.send(self.id, self.exchange_id, MessageType::CloseOrder, payload);
                    self.has_long_position = false;
                }
            }
            3 => {
                // Open Short
                let payload = MessagePayload::MarketOrder(MarketOrderPayload {
                    symbol: self.symbol.clone(),
                    side: Side::Sell,
                    qty,
                    leverage: 5, // default 5x leverage
                });
                println!("[Trader {}] OPEN SHORT 5x qty={}", self.name, qty);
                sim.send(self.id, self.exchange_id, MessageType::MarketOrder, payload);
                self.has_short_position = true;
            }
            0 => {
                // Close Short
                if self.has_short_position {
                    let payload = MessagePayload::CloseOrder(CloseOrderPayload {
                        symbol: self.symbol.clone(),
                        side: Side::Sell,
                    });
                    println!("[Trader {}] CLOSE SHORT", self.name);
                    sim.send(self.id, self.exchange_id, MessageType::CloseOrder, payload);
                    self.has_short_position = false;
                }
            }
            _ => {}
        }

        // Schedule next wakeup
        let next = now_ns.saturating_add(self.wake_interval_ns);
        sim.wakeup(self.id, next);
    }

    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, msg: &Message) {
        if let MessageType::PositionLiquidated = msg.msg_type {
            if let MessagePayload::PositionLiquidated(PositionLiquidatedPayload {
                symbol,
                side,
                pnl,
                collateral_lost,
                ..
            }) = &msg.payload
            {
                let side_str = match side {
                    Side::Buy => "LONG",
                    Side::Sell => "SHORT",
                };
                println!(
                    "[Trader {}] ⚠️ LIQUIDATED {} {} pnl=${:.2} lost=${:.2}",
                    self.name,
                    symbol,
                    side_str,
                    *pnl as f64 / 1_000_000.0,
                    *collateral_lost as f64 / 1_000_000.0
                );
                // Reset position tracking
                match side {
                    Side::Buy => self.has_long_position = false,
                    Side::Sell => self.has_short_position = false,
                }
            }
        }
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!("[Trader {}] stopping, trades={}", self.name, self.trade_count);
    }
}
