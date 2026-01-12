//! Liquidation Agent
//!
//! Periodically scans all open positions and triggers liquidation for underwater positions.
//! This agent wakes up at a fixed interval (e.g., 200ms) and sends liquidation scan requests
//! to the Exchange agent.

use crate::agents::Agent;
use crate::messages::{
    AgentId, Message, MessagePayload, MessageType, LiquidationTaskPayload, SimulatorApi,
};

pub struct LiquidationAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    wake_interval_ns: u64,
    scan_count: u64,
}

impl LiquidationAgent {
    /// Create a new LiquidationAgent
    /// 
    /// # Arguments
    /// * `id` - Unique agent identifier
    /// * `name` - Agent name for logging
    /// * `exchange_id` - The exchange agent to send liquidation requests to
    /// * `wake_interval_ns` - How often to scan for liquidations (in nanoseconds)
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
        }
    }

    /// Scan for liquidatable positions
    fn scan_liquidations(&mut self, sim: &mut dyn SimulatorApi) {
        self.scan_count += 1;

        // Log every 10th scan to reduce noise
        let verbose = self.scan_count <= 3 || self.scan_count % 10 == 0;

        if verbose {
            println!(
                "[Liquidation {}] Scan #{} at t={} ns",
                self.name,
                self.scan_count,
                sim.now_ns()
            );
        }

        // Send liquidation scan request to exchange
        // The exchange will check all positions and execute liquidations if needed
        let payload = MessagePayload::LiquidationTask(LiquidationTaskPayload {
            symbol: "ALL".to_string(), // Scan all symbols
            max_positions: 1000, // Max positions to check in one scan
        });

        sim.send(
            self.id,
            self.exchange_id,
            MessageType::LiquidationScan,
            payload,
        );
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
            "[Liquidation {}] starting with scan interval {}ms -> exchange={}",
            self.name,
            self.wake_interval_ns / 1_000_000,
            self.exchange_id
        );

        // Schedule first wakeup
        let now = sim.now_ns();
        let next = now + self.wake_interval_ns;
        sim.wakeup(self.id, next);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        // Scan for liquidations
        self.scan_liquidations(sim);

        // Schedule next wakeup
        let next = now_ns + self.wake_interval_ns;
        sim.wakeup(self.id, next);
    }

    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, msg: &Message) {
        // Liquidation agent doesn't need to handle messages currently
        // Could be extended to receive liquidation results from exchange
        if msg.msg_type != MessageType::Wakeup {
            println!(
                "[Liquidation {}] received unexpected msg {:?} from {}",
                self.name, msg.msg_type, msg.from
            );
        }
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!(
            "[Liquidation {}] stopping after {} scans",
            self.name, self.scan_count
        );
    }
}

