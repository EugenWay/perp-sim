use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::time::{Duration, Instant};

use crate::agents::Agent;
use crate::events::{EventBus, SimEvent};
use crate::latency::LatencyModel;
use crate::messages::{AgentId, Message, MessagePayload, MessageType, SimulatorApi};

// BinaryHeap wrapper - inverted ordering for min-heap by timestamp
#[derive(Clone)]
struct ScheduledMessage(Message);

impl Eq for ScheduledMessage {}

impl PartialEq for ScheduledMessage {
    fn eq(&self, other: &Self) -> bool {
        self.0.at == other.0.at
    }
}

impl Ord for ScheduledMessage {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering: smaller `at` = "greater" priority
        other.0.at.cmp(&self.0.at)
    }
}

impl PartialOrd for ScheduledMessage {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Simulation kernel.
/// Owns the agents, virtual time, the message queue and the EventBus.
pub struct Kernel {
    time_ns: u64,
    tick_ns: u64,
    latency: Box<dyn LatencyModel>,
    queue: BinaryHeap<ScheduledMessage>,
    agents: Vec<Box<dyn Agent>>,
    /// O(1) lookup: AgentId -> index in agents vec
    agent_index: HashMap<AgentId, usize>,
    event_bus: EventBus,
    /// If Some, run in realtime mode with this delay between ticks
    realtime_tick_ms: Option<u64>,
}

impl Kernel {
    /// Create a new kernel with given latency model and time step.
    /// Automatically uses current system time as starting point.
    pub fn new(latency: Box<dyn LatencyModel>, tick_ns: u64) -> Self {
        // Get current Unix timestamp in nanoseconds
        let time_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("System time before Unix epoch")
            .as_nanos() as u64;

        Self {
            time_ns,
            tick_ns,
            latency,
            queue: BinaryHeap::new(),
            agents: Vec::new(),
            agent_index: HashMap::new(),
            event_bus: EventBus::new(),
            realtime_tick_ms: None,
        }
    }

    /// Enable realtime mode with specified tick interval in milliseconds.
    pub fn set_realtime(&mut self, tick_ms: u64) {
        self.realtime_tick_ms = Some(tick_ms);
        println!("[Kernel] realtime mode enabled: {}ms per tick", tick_ms);
    }

    /// Access to the event bus (for SimEngine to subscribe loggers).
    pub fn event_bus_mut(&mut self) -> &mut EventBus {
        &mut self.event_bus
    }

    /// Add a new agent into the simulation.
    pub fn add_agent(&mut self, mut agent: Box<dyn Agent>) {
        let id = agent.id();
        println!("[Kernel] registering agent {} (id={})", agent.name(), id);
        // Let the agent initialize itself using the simulator API.
        agent.on_start(self);
        let idx = self.agents.len();
        self.agents.push(agent);
        self.agent_index.insert(id, idx);
    }

    /// Run the simulation for `max_steps` ticks, or until the queue is empty.
    pub fn run(&mut self, max_steps: usize) {
        println!(
            "[Kernel] starting simulation with {} agents, tick_ns = {}",
            self.agents.len(),
            self.tick_ns
        );
        println!("[Kernel] start time: {} ns", self.time_ns);
        if let Some(ms) = self.realtime_tick_ms {
            println!("[Kernel] REALTIME MODE: {}ms between ticks", ms);
        }

        for step in 0..max_steps {
            let tick_start = Instant::now();

            // Advance virtual time.
            self.time_ns = self.time_ns.saturating_add(self.tick_ns);

            // In realtime mode, print less verbose output
            if self.realtime_tick_ms.is_some() {
                if step % 10 == 0 {
                    println!("[Kernel] TICK {} (realtime)", step + 1);
                }
            } else {
            println!("\n[Kernel] === TICK {} at t={} ns ===", step + 1, self.time_ns);
            }

            // Deliver all messages whose delivery time is <= now.
            loop {
                let next_at = match self.queue.peek() {
                    Some(sm) => sm.0.at,
                    None => break,
                };

                if next_at > self.time_ns {
                    break;
                }

                let sm = self.queue.pop().expect("queue was not empty");
                let msg = sm.0;
                let target = msg.to;

                // O(1) lookup using agent_index
                let idx_opt = self.agent_index.get(&target).copied();

                if let Some(idx) = idx_opt {
                    // Temporarily move agent out of the vector to avoid
                    // aliasing &mut self and &mut agent at the same time.
                    let mut agent = self.agents.remove(idx);

                    {
                        // Use `self` as SimulatorApi while the agent is detached.
                        let sim: &mut dyn SimulatorApi = self;
                        match msg.msg_type {
                            MessageType::Wakeup => agent.on_wakeup(sim, msg.at),
                            _ => agent.on_message(sim, &msg),
                        }
                    }

                    // Put the agent back in the same position.
                    self.agents.insert(idx, agent);
                } else {
                    println!(
                        "[Kernel] message scheduled for unknown agent id={} -> dropped: {:?}",
                        target, msg
                    );
                }
            }

            if self.queue.is_empty() {
                println!("\n[Kernel] queue is empty, stopping early after {} ticks", step + 1);
                break;
            }

            // Realtime mode: wait for the remaining time of this tick
            if let Some(tick_ms) = self.realtime_tick_ms {
                let elapsed = tick_start.elapsed();
                let target = Duration::from_millis(tick_ms);
                if elapsed < target {
                    std::thread::sleep(target - elapsed);
                }
            }
        }

        // Notify agents that we are stopping.
        for _ in 0..self.agents.len() {
            let mut agent = self.agents.remove(0);
            agent.on_stop(self);
            self.agents.push(agent);
        }

        println!("[Kernel] simulation finished at {} ns", self.time_ns);
    }
}

impl SimulatorApi for Kernel {
    fn now_ns(&self) -> u64 {
        self.time_ns
    }

    fn send(&mut self, from: AgentId, to: AgentId, kind: MessageType, payload: MessagePayload) {
        let network = self.latency.delay_ns(from, to);
        let compute = self.latency.compute_ns(to);
        let at = self.time_ns.saturating_add(network).saturating_add(compute);

        let msg = Message {
            to,
            from,
            msg_type: kind,
            at,
            payload,
        };

        // --- EventBus: generate high-level events ---
        match msg.msg_type {
            MessageType::LimitOrder
            | MessageType::MarketOrder
            | MessageType::CancelOrder
            | MessageType::ModifyOrder => {
                // Extract symbol/side/price/qty for CSV logging
                let (symbol, side, price, qty) = match &msg.payload {
                    MessagePayload::LimitOrder(p) => (Some(p.symbol.clone()), Some(p.side), Some(p.price), Some(p.qty)),
                    MessagePayload::MarketOrder(p) => (Some(p.symbol.clone()), Some(p.side), None, Some(p.qty)),
                    _ => (None, None, None, None),
                };

                let ev = SimEvent::OrderLog {
                    ts: self.time_ns,
                    from,
                    to,
                    msg_type: kind,
                    symbol,
                    side,
                    price,
                    qty,
                };
                self.event_bus.emit(ev);
            }

            MessageType::OracleTick => {
                if let MessagePayload::OracleTick(p) = &msg.payload {
                    let ev = SimEvent::OracleTick {
                        ts: self.time_ns,
                        symbol: p.symbol.clone(),
                        price_min: p.price.min,
                        price_max: p.price.max,
                    };
                    self.event_bus.emit(ev);
                }
            }

            _ => {
                // Optionally emit "RawMessage":
                // self.event_bus.emit(SimEvent::RawMessage { ts: self.time_ns, msg: msg.clone() });
            }
        }
        // --- End of EventBus block ---

        self.queue.push(ScheduledMessage(msg));
    }

    fn wakeup(&mut self, agent_id: AgentId, at_ns: u64) {
        let msg = Message::new_empty(agent_id, agent_id, MessageType::Wakeup, at_ns);
        self.queue.push(ScheduledMessage(msg));
    }

    fn broadcast(&mut self, from: AgentId, kind: MessageType, payload: MessagePayload) {
        // Emit event once for broadcast (not per recipient)
        if let MessageType::OracleTick = kind {
            if let MessagePayload::OracleTick(p) = &payload {
                let ev = SimEvent::OracleTick {
                    ts: self.time_ns,
                    symbol: p.symbol.clone(),
                    price_min: p.price.min,
                    price_max: p.price.max,
                };
                self.event_bus.emit(ev);
            }
        }

        for i in 0..self.agents.len() {
            let id = self.agents[i].id();
            if id == from {
                continue;
            }
            let network = self.latency.delay_ns(from, id);
            let compute = self.latency.compute_ns(id);
            let at = self.time_ns.saturating_add(network).saturating_add(compute);
            let msg = Message {
                to: id,
                from,
                msg_type: kind,
                at,
                payload: payload.clone(),
            };

            self.queue.push(ScheduledMessage(msg));
        }
    }

    fn emit_event(&mut self, event: SimEvent) {
        self.event_bus.emit(event);
    }
}
