use std::collections::HashMap;
use crate::messages::{AgentId, OrderId, OrderPayload};

const DEFAULT_TTL_SEC: u64 = 24 * 3600;

#[derive(Debug, Clone)]
pub struct PendingOrder {
    pub id: OrderId,
    pub owner: AgentId,
    pub payload: OrderPayload,
    #[allow(dead_code)]
    pub created_at_ns: u64,
    pub valid_until_ns: u64,
    #[allow(dead_code)]
    pub position_entry_price: Option<u64>,
}

pub struct PendingOrderStore {
    orders: HashMap<OrderId, PendingOrder>,
    next_id: OrderId,
    by_owner: HashMap<AgentId, Vec<OrderId>>,
    by_symbol: HashMap<String, Vec<OrderId>>,
}

impl PendingOrderStore {
    pub fn new() -> Self {
        Self {
            orders: HashMap::new(),
            next_id: 1,
            by_owner: HashMap::new(),
            by_symbol: HashMap::new(),
        }
    }

    pub fn insert(&mut self, owner: AgentId, payload: OrderPayload, now_ns: u64) -> OrderId {
        let id = self.next_id;
        self.next_id += 1;

        let ttl_sec = payload.valid_for_sec.unwrap_or(DEFAULT_TTL_SEC);
        let valid_until_ns = now_ns + ttl_sec * 1_000_000_000;

        let order = PendingOrder {
            id,
            owner,
            payload: payload.clone(),
            created_at_ns: now_ns,
            valid_until_ns,
            position_entry_price: None,
        };

        self.by_owner.entry(owner).or_default().push(id);
        self.by_symbol.entry(payload.symbol.clone()).or_default().push(id);
        self.orders.insert(id, order);

        id
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
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.orders.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn get_by_owner(&self, owner: AgentId) -> Vec<&PendingOrder> {
        self.by_owner
            .get(&owner)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.orders.get(id))
                    .collect()
            })
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
