use crate::agents::Agent;
use crate::api::PriceProvider;
use crate::messages::{AgentId, Message, MessagePayload, MessageType, OracleTickPayload, Price, SimulatorApi};

pub struct OracleAgent {
    id: AgentId,
    name: String,
    symbols: Vec<String>,
    exchange_id: AgentId,
    wake_interval_ns: u64,
    block_number: u64,
    price_provider: Box<dyn PriceProvider>,
}

impl OracleAgent {
    pub fn new(
        id: AgentId,
        name: String,
        symbols: Vec<String>,
        exchange_id: AgentId,
        wake_interval_ns: u64,
        price_provider: Box<dyn PriceProvider>,
    ) -> Self {
        Self {
            id,
            name,
            symbols,
            exchange_id,
            wake_interval_ns,
            block_number: 0,
            price_provider,
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
            "[Oracle {}] starting with provider '{}' for {} symbols -> exchange={}",
            self.name,
            self.price_provider.provider_name(),
            self.symbols.len(),
            self.exchange_id
        );
        println!("[Oracle {}] symbols: {}", self.name, self.symbols.join(", "));
        println!(
            "[Oracle {}] wake interval: {}s",
            self.name,
            self.wake_interval_ns / 1_000_000_000
        );

        let now = sim.now_ns();
        let next = now + self.wake_interval_ns;
        sim.wakeup(self.id, next);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        self.block_number += 1;

        println!(
            "\n[Oracle {}] ========== BLOCK #{} at t={} ns ==========",
            self.name, self.block_number, now_ns
        );

        let symbol_refs: Vec<&str> = self.symbols.iter().map(|s| s.as_str()).collect();
        let results = self.price_provider.fetch_batch(&symbol_refs);

        for (symbol, result) in self.symbols.iter().zip(results.into_iter()) {
            match result {
                Ok(signed_data) => {
                    let price_usd = signed_data.price_usd_micro as f64 / 1_000_000.0;
                    let confidence_usd = signed_data.confidence.map(|c| c as f64 / 1_000_000.0).unwrap_or(0.0);
                    let confidence = signed_data.confidence.unwrap_or(0);

                    // Compute min/max from confidence interval (price ± confidence)
                    let min = signed_data.price_usd_micro.saturating_sub(confidence);
                    let max = signed_data.price_usd_micro.saturating_add(confidence);

                    println!(
                        "[Oracle {}] {} = ${:.2} (±${:.2}) [${:.2}-${:.2}] [{}] sig:{} bytes",
                        self.name,
                        symbol,
                        price_usd,
                        confidence_usd,
                        min as f64 / 1_000_000.0,
                        max as f64 / 1_000_000.0,
                        signed_data.provider_name,
                        signed_data.signature.len()
                    );

                    let payload = MessagePayload::OracleTick(OracleTickPayload {
                        symbol: symbol.clone(),
                        price: Price { min, max },
                        publish_time: signed_data.publish_time,
                        signature: signed_data.signature,
                    });

                    sim.send(self.id, self.exchange_id, MessageType::OracleTick, payload);
                }
                Err(e) => {
                    eprintln!("[Oracle {}] error fetching {}: {}", self.name, symbol, e);
                }
            }
        }

        let next = now_ns + self.wake_interval_ns;
        sim.wakeup(self.id, next);
    }

    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, msg: &Message) {
        println!(
            "[Oracle {}] received msg {:?} from {}",
            self.name, msg.msg_type, msg.from
        );
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!("[Oracle {}] stopping after {} blocks", self.name, self.block_number);
    }
}
