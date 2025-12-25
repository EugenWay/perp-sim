use crate::agents::Agent;
use crate::messages::{
    AgentId, CloseOrderPayload, MarketOrderPayload, Message, MessagePayload, MessageType, Side, SimulatorApi,
};

pub struct TraderAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    symbol: String,
    wake_interval_ns: u64,
    // State tracking
    has_long_position: bool,
    has_short_position: bool,
    trade_count: u32,
}

impl TraderAgent {
    pub fn new(id: AgentId, name: String, exchange_id: AgentId, symbol: String) -> Self {
        Self {
            id,
            name,
            exchange_id,
            symbol,
            wake_interval_ns: 2_000_000_000, // 2 seconds
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

        match action {
            1 => {
                // Open Long
                let payload = MessagePayload::MarketOrder(MarketOrderPayload {
                    symbol: self.symbol.clone(),
                    side: Side::Buy,
                    qty: 1,
                });
                println!("[Trader {}] OPEN LONG", self.name);
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
                    qty: 1,
                });
                println!("[Trader {}] OPEN SHORT", self.name);
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
        println!("[Trader {}] received {:?} from {}", self.name, msg.msg_type, msg.from);
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!("[Trader {}] stopping, trades={}", self.name, self.trade_count);
    }
}
