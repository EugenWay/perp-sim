use crate::messages::{AgentId, MessageType, Side};

#[derive(Debug, Clone)]
pub enum SimEvent {
    OrderLog {
        ts: u64,
        from: AgentId,
        to: AgentId,
        msg_type: MessageType,
        symbol: Option<String>,
        side: Option<Side>,
        price: Option<u64>,
        qty: Option<u64>,
    },

    OracleTick {
        ts: u64,
        symbol: String,
        price_min: u64,
        price_max: u64,
    },
}

pub trait EventListener {
    fn on_event(&mut self, event: &SimEvent);
}

pub struct EventBus {
    listeners: Vec<Box<dyn EventListener>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self { listeners: Vec::new() }
    }

    /// Subscribe a new listener.
    pub fn subscribe(&mut self, listener: Box<dyn EventListener>) {
        self.listeners.push(listener);
    }

    /// Emit an event to all listeners.
    pub fn emit(&mut self, event: SimEvent) {
        for listener in self.listeners.iter_mut() {
            listener.on_event(&event);
        }
    }
}
