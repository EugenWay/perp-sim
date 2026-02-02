use crate::agents::Agent;
use crate::messages::{AgentId, Message, MessagePayload, MessageType, SimulatorApi};

pub struct LiquidationAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    wake_interval_ns: u64,
    scan_count: u64,
    liquidations_triggered: u64,
}

impl LiquidationAgent {
    pub fn new(
        id: AgentId,
        name: String,
        exchange_id: AgentId,
        wake_interval_ns: u64,
    ) -> Self {
        Self {
            id,
            name,
            exchange_id,
            wake_interval_ns,
            scan_count: 0,
            liquidations_triggered: 0,
        }
    }
}

impl Agent for LiquidationAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn on_start(&mut self, sim: &mut dyn SimulatorApi) {
        println!(
            "[Liquidation {}] starting (interval={}ms)",
            self.name,
            self.wake_interval_ns / 1_000_000,
        );
        sim.wakeup(self.id, sim.now_ns() + self.wake_interval_ns);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        self.scan_count += 1;
        sim.send(self.id, self.exchange_id, MessageType::LiquidationScan, MessagePayload::Empty);
        sim.wakeup(self.id, now_ns + self.wake_interval_ns);
    }

    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, msg: &Message) {
        if msg.msg_type == MessageType::PositionLiquidated {
            self.liquidations_triggered += 1;
        }
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!(
            "[Liquidation {}] stopping after {} scans, {} liquidations triggered",
            self.name, self.scan_count, self.liquidations_triggered
        );
    }
}
