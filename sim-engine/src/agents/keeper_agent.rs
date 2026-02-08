use crate::agents::Agent;
use crate::messages::{
    AgentId, ExecuteOrderPayload, KeeperRewardPayload, Message, MessagePayload, MessageType, OracleTickPayload,
    PendingOrderInfo, PendingOrdersListPayload, Price, SimulatorApi,
};
use crate::trigger_checker;
use std::collections::HashMap;

pub struct KeeperAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    wake_interval_ns: u64,

    prices: HashMap<String, Price>,
    pending_orders: Vec<PendingOrderInfo>,

    orders_executed: u32,
    orders_missed: u32,
    total_rewards: u64,
}

#[derive(Debug, Clone)]
pub struct KeeperConfig {
    pub name: String,
    pub exchange_id: AgentId,
    pub address: Option<String>,
    pub wake_interval_ms: u64,
}

impl KeeperAgent {
    pub fn new(id: AgentId, config: KeeperConfig) -> Self {
        Self {
            id,
            name: config.name,
            exchange_id: config.exchange_id,
            wake_interval_ns: config.wake_interval_ms * 1_000_000,
            prices: HashMap::new(),
            pending_orders: Vec::new(),
            orders_executed: 0,
            orders_missed: 0,
            total_rewards: 0,
        }
    }

    fn check_and_execute_triggers(&mut self, sim: &mut dyn SimulatorApi) {
        for order in &self.pending_orders {
            if let Some(price) = self.prices.get(&order.symbol) {
                if trigger_checker::is_triggered_info(order, price) {
                    println!(
                        "[Keeper {}] Triggering order #{} {}",
                        self.name, order.order_id, order.symbol
                    );

                    sim.send(
                        self.id,
                        self.exchange_id,
                        MessageType::ExecuteOrder,
                        MessagePayload::ExecuteOrder(ExecuteOrderPayload {
                            order_id: order.order_id,
                        }),
                    );
                }
            }
        }
    }
}

impl Agent for KeeperAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn on_start(&mut self, sim: &mut dyn SimulatorApi) {
        println!(
            "[Keeper {}] Started (interval={}ms)",
            self.name,
            self.wake_interval_ns / 1_000_000,
        );
        sim.wakeup(self.id, sim.now_ns() + self.wake_interval_ns);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        sim.send(
            self.id,
            self.exchange_id,
            MessageType::GetPendingOrders,
            MessagePayload::Empty,
        );
        sim.wakeup(self.id, now_ns + self.wake_interval_ns);
    }

    fn on_message(&mut self, sim: &mut dyn SimulatorApi, msg: &Message) {
        match msg.msg_type {
            MessageType::OracleTick => {
                if let MessagePayload::OracleTick(OracleTickPayload { symbol, price, .. }) = &msg.payload {
                    self.prices.insert(symbol.clone(), *price);
                    self.check_and_execute_triggers(sim);
                }
            }

            MessageType::PendingOrdersList => {
                if let MessagePayload::PendingOrdersList(PendingOrdersListPayload { orders }) = &msg.payload {
                    self.pending_orders = orders.clone();
                    self.check_and_execute_triggers(sim);
                }
            }

            MessageType::KeeperReward => {
                if let MessagePayload::KeeperReward(KeeperRewardPayload {
                    order_id,
                    reward_micro_usd,
                }) = &msg.payload
                {
                    self.orders_executed += 1;
                    self.total_rewards += reward_micro_usd;
                    println!(
                        "[Keeper {}] REWARD #{}: ${:.4}",
                        self.name,
                        order_id,
                        *reward_micro_usd as f64 / 1_000_000.0
                    );
                }
            }

            MessageType::OrderAlreadyExecuted => {
                self.orders_missed += 1;
            }

            _ => {}
        }
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!(
            "[Keeper {}] STOP: executed={} missed={} rewards=${:.2}",
            self.name,
            self.orders_executed,
            self.orders_missed,
            self.total_rewards as f64 / 1_000_000.0
        );
    }
}
