use crate::messages::{AgentId, Message, SimulatorApi};

pub mod exchange_agent;
pub mod human_agent;
pub mod keeper_agent;
pub mod limit_trader_agent;
pub mod liquidation_agent;
pub mod market_maker_agent;
pub mod oracle_agent;
pub mod smart_trader_agent;

pub use keeper_agent::{KeeperAgent, KeeperConfig};
pub use limit_trader_agent::{LimitTraderAgent, LimitTraderConfig, LimitStrategy, OrderMode};
pub use market_maker_agent::{MarketMakerAgent, MarketMakerConfig};

pub trait Agent {
    fn id(&self) -> AgentId;
    fn name(&self) -> &str;
    fn on_start(&mut self, _sim: &mut dyn SimulatorApi) {}
    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {}
    fn on_wakeup(&mut self, _sim: &mut dyn SimulatorApi, _now_ns: u64) {}
    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, _msg: &Message) {}
}
