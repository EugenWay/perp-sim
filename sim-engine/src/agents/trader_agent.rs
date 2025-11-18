// src/agents/trader_agent.rs
// Simple trader agent that occasionally sends market orders to the exchange.

use crate::agents::Agent;
use crate::messages::{AgentId, MarketOrderPayload, Message, MessagePayload, MessageType, Side, SimulatorApi};

pub struct TraderAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    symbol: String,
    wake_interval_ns: u64,
    last_side_long: bool,
}

impl TraderAgent {
    pub fn new(id: AgentId, name: String, exchange_id: AgentId, symbol: String) -> Self {
        Self {
            id,
            name,
            exchange_id,
            symbol,
            wake_interval_ns: 2_000_000_000, // 2 seconds in virtual time
            last_side_long: true,
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
        // Flip side each wakeup just for demo.
        self.last_side_long = !self.last_side_long;
        let side = if self.last_side_long { Side::Buy } else { Side::Sell };

        let payload = MessagePayload::MarketOrder(MarketOrderPayload {
            symbol: self.symbol.clone(),
            side,
            qty: 1,
        });

        println!(
            "[Trader {}] wakeup at t={} ns -> sending MARKET_ORDER side={:?}",
            self.name, now_ns, side
        );

        sim.send(self.id, self.exchange_id, MessageType::MarketOrder, payload);

        // Schedule next wakeup.
        let next = now_ns.saturating_add(self.wake_interval_ns);
        sim.wakeup(self.id, next);
    }

    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, msg: &Message) {
        println!(
            "[Trader {}] received msg {:?} from {} payload={:?}",
            self.name, msg.msg_type, msg.from, msg.payload
        );
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!("[Trader {}] stopping", self.name);
    }
}
