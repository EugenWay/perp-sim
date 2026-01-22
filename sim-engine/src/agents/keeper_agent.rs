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

    orders_executed: u32,
    orders_missed: u32,
    total_rewards: u64,
    liquidations_triggered: u32,
}

#[derive(Debug, Clone)]
pub struct KeeperConfig {
    pub name: String,
    pub exchange_id: AgentId,
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
            orders_executed: 0,
            orders_missed: 0,
            total_rewards: 0,
            liquidations_triggered: 0,
        }
    }

    fn check_trigger(&self, order: &PendingOrderInfo) -> bool {
        match self.prices.get(&order.symbol) {
            Some(price) => trigger_checker::is_triggered_info(order, price),
            None => false,
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
            self.wake_interval_ns / 1_000_000
        );
        sim.wakeup(self.id, sim.now_ns() + self.wake_interval_ns);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        // Request pending orders list
        sim.send(
            self.id,
            self.exchange_id,
            MessageType::GetPendingOrders,
            MessagePayload::Empty,
        );

        // Also trigger liquidation scan
        sim.send(
            self.id,
            self.exchange_id,
            MessageType::LiquidationScan,
            MessagePayload::Empty,
        );

        sim.wakeup(self.id, now_ns + self.wake_interval_ns);
    }

    fn on_message(&mut self, sim: &mut dyn SimulatorApi, msg: &Message) {
        match msg.msg_type {
            MessageType::OracleTick => {
                if let MessagePayload::OracleTick(OracleTickPayload { symbol, price, .. }) = &msg.payload {
                    self.prices.insert(symbol.clone(), *price);
                }
            }

            MessageType::PendingOrdersList => {
                if let MessagePayload::PendingOrdersList(PendingOrdersListPayload { orders }) = &msg.payload {
                    for order_info in orders {
                        if self.check_trigger(order_info) {
                            println!(
                                "[Keeper {}] TRIGGER #{} {} {:?}",
                                self.name, order_info.order_id, order_info.symbol, order_info.side
                            );

                            sim.send(
                                self.id,
                                self.exchange_id,
                                MessageType::ExecuteOrder,
                                MessagePayload::ExecuteOrder(ExecuteOrderPayload {
                                    order_id: order_info.order_id,
                                }),
                            );
                        }
                    }
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

            MessageType::PositionLiquidated => {
                self.liquidations_triggered += 1;
            }

            _ => {}
        }
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!(
            "[Keeper {}] STOP: executed={} missed={} liquidations={} rewards=${:.2}",
            self.name,
            self.orders_executed,
            self.orders_missed,
            self.liquidations_triggered,
            self.total_rewards as f64 / 1_000_000.0
        );
    }
}
