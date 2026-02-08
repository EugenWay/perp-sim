use std::collections::HashMap;
use crate::messages::{AgentId, OrderId, OrderPayload};

#[derive(Debug, Clone)]
pub struct PendingOrder {
    pub id: OrderId,
    pub owner: AgentId,
    pub payload: OrderPayload,
    pub valid_until_ns: u64,
}

pub struct PendingOrderStore {
    orders: HashMap<OrderId, PendingOrder>,
    by_owner: HashMap<AgentId, Vec<OrderId>>,
    by_symbol: HashMap<String, Vec<OrderId>>,
}

impl PendingOrderStore {
    pub fn new() -> Self {
        Self {
            orders: HashMap::new(),
            by_owner: HashMap::new(),
            by_symbol: HashMap::new(),
        }
    }

    pub fn remove(&mut self, order_id: OrderId) -> Option<PendingOrder> {
        let order = self.orders.remove(&order_id)?;
        if let Some(ids) = self.by_owner.get_mut(&order.owner) {
            ids.retain(|&id| id != order_id);
        }
        if let Some(ids) = self.by_symbol.get_mut(&order.payload.symbol) {
            ids.retain(|&id| id != order_id);
        }
        Some(order)
    }

    pub fn get(&self, order_id: OrderId) -> Option<&PendingOrder> {
        self.orders.get(&order_id)
    }

    pub fn get_by_symbol(&self, symbol: &str) -> Vec<&PendingOrder> {
        self.by_symbol
            .get(symbol)
            .map(|ids| ids.iter().filter_map(|id| self.orders.get(id)).collect())
            .unwrap_or_default()
    }

    pub fn remove_expired(&mut self, now_ns: u64) -> Vec<PendingOrder> {
        let expired_ids: Vec<OrderId> = self.orders
            .iter()
            .filter(|(_, o)| o.valid_until_ns <= now_ns)
            .map(|(&id, _)| id)
            .collect();
        expired_ids.iter().filter_map(|&id| self.remove(id)).collect()
    }
}

impl Default for PendingOrderStore {
    fn default() -> Self {
        Self::new()
    }
}
