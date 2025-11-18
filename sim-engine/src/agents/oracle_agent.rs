// src/agents/oracle_agent.rs
// Oracle bot stub: periodically sends OracleTick messages to the exchange.

use crate::agents::Agent;
use crate::messages::{AgentId, Message, MessagePayload, MessageType, OracleTickPayload, SimulatorApi};

/// Simple oracle bot that ticks price over time.
pub struct OracleAgent {
    id: AgentId,
    name: String,
    symbol: String,
    exchange_id: AgentId,
    tick: u64,
    base_price: u64,
}

impl OracleAgent {
    pub fn new(id: AgentId, name: String, symbol: String, exchange_id: AgentId) -> Self {
        Self {
            id,
            name,
            symbol,
            exchange_id,
            tick: 0,
            base_price: 1500,
        }
    }
}

impl Agent for OracleAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn on_start(&mut self, sim: &mut dyn SimulatorApi) {
        println!(
            "[Oracle {}] starting for symbol {} -> exchange={}",
            self.name, self.symbol, self.exchange_id
        );

        // Schedule first wakeup immediately.
        let now = sim.now_ns();
        sim.wakeup(self.id, now);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        self.tick += 1;
        let price = self.base_price + self.tick * 10;

        println!(
            "[Oracle {}] wakeup at t={} ns -> ORACLE_TICK price={}",
            self.name, now_ns, price
        );

        let payload = MessagePayload::OracleTick(OracleTickPayload {
            symbol: self.symbol.clone(),
            price,
        });

        sim.send(self.id, self.exchange_id, MessageType::OracleTick, payload);

        // Schedule next wakeup 1 second later in virtual time.
        let next = now_ns.saturating_add(1_000_000_000);
        sim.wakeup(self.id, next);
    }

    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, msg: &Message) {
        println!(
            "[Oracle {}] received msg {:?} from {} payload={:?}",
            self.name, msg.msg_type, msg.from, msg.payload
        );
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!("[Oracle {}] stopping", self.name);
    }
}
