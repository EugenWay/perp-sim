// src/agents/exchange_agent.rs
// PerpExchange adapter agent (stub).
// Later this will wrap the real PerpExchange engine / contract API.

use crate::agents::Agent;
use crate::messages::{AgentId, Message, MessagePayload, MessageType, OracleTickPayload, SimulatorApi};

/// Simple stub for a PerpExchange agent.
/// Right now it just logs incoming messages and stores last oracle price.
pub struct ExchangeAgent {
    id: AgentId,
    name: String,
    symbol: String,
    last_price: Option<u64>,
}

impl ExchangeAgent {
    pub fn new(id: AgentId, name: String, symbol: String) -> Self {
        Self {
            id,
            name,
            symbol,
            last_price: None,
        }
    }

    pub fn last_price(&self) -> Option<u64> {
        self.last_price
    }
}

impl Agent for ExchangeAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn on_start(&mut self, _sim: &mut dyn SimulatorApi) {
        println!("[Exchange {}] starting for symbol {}", self.name, self.symbol);
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!("[Exchange {}] stopping", self.name);
    }

    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, msg: &Message) {
        match msg.msg_type {
            MessageType::OracleTick => {
                if let MessagePayload::OracleTick(OracleTickPayload { symbol, price }) = &msg.payload {
                    if symbol == &self.symbol {
                        self.last_price = Some(*price);
                        println!(
                            "[Exchange {}] ORACLE_TICK {} price={} from {}",
                            self.name, symbol, price, msg.from
                        );
                    } else {
                        println!(
                            "[Exchange {}] ignored oracle tick for other symbol {} from {}",
                            self.name, symbol, msg.from
                        );
                    }
                } else {
                    println!(
                        "[Exchange {}] malformed OracleTick payload from {}",
                        self.name, msg.from
                    );
                }
            }

            MessageType::MarketOrder | MessageType::LimitOrder => {
                println!(
                    "[Exchange {}] order from {}: type={:?}, payload={:?}",
                    self.name, msg.from, msg.msg_type, msg.payload
                );
                // TODO: route into PerpExchange engine.
            }

            MessageType::LiquidationScan => {
                println!(
                    "[Exchange {}] LIQUIDATION_SCAN from {}: {:?} (stub)",
                    self.name, msg.from, msg.payload
                );
            }

            _ => {
                println!(
                    "[Exchange {}] msg {:?} from {} payload={:?}",
                    self.name, msg.msg_type, msg.from, msg.payload
                );
            }
        }
    }
}
