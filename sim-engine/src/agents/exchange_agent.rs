use crate::agents::Agent;
use crate::events::SimEvent;
use crate::messages::{
    AgentId, CloseOrderPayload, MarketOrderPayload, Message, MessagePayload, MessageType, OracleTickPayload,
    Side as SimSide, SimulatorApi,
};
use perp_futures::executor::Executor;
use perp_futures::oracle::Oracle;
use perp_futures::services::BasicServicesBundle;
use perp_futures::state::PositionKey;
use perp_futures::state::State;
use perp_futures::types::{
    AccountId, AssetId, MarketId, OraclePrices, Order, OrderType, Side, Timestamp, TokenAmount, Usd,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

// ==== Market Configuration (from scenario JSON) ====

#[derive(Debug, Clone)]
pub struct MarketConfig {
    pub id: u32,
    pub symbol: String,
    pub index_token: String,
    pub collateral_token: String,
    pub collateral_amount: i128,
    pub index_amount: i128,
    pub liquidity_usd: i128,
}

// ==== SimOracle: adapter for perp-futures Oracle trait ====

#[derive(Clone)]
struct PriceCache {
    /// Maps symbol -> (index_price_min, index_price_max) in micro-dollars (1e6 = $1)
    prices: HashMap<String, (Usd, Usd)>,
}

impl PriceCache {
    fn new() -> Self {
        Self { prices: HashMap::new() }
    }

    fn update(&mut self, symbol: &str, min: u64, max: u64) {
        self.prices.insert(symbol.to_string(), (min as Usd, max as Usd));
    }

    fn get(&self, symbol: &str) -> Option<(Usd, Usd)> {
        self.prices.get(symbol).copied()
    }
}

/// Oracle implementation that reads prices from a shared cache
pub struct SimOracle {
    cache: Rc<RefCell<PriceCache>>,
    market_symbols: HashMap<MarketId, String>,
    collateral_price: Usd,
}

impl SimOracle {
    fn new(cache: Rc<RefCell<PriceCache>>, markets: &[MarketConfig]) -> Self {
        let mut market_symbols = HashMap::new();
        for m in markets {
            market_symbols.insert(MarketId(m.id), m.symbol.clone());
        }

        Self {
            cache,
            market_symbols,
            // collateral_price = 1 because our tokens are already in micro-USD
            // (1 token = $0.000001, so 1_000_000 tokens = $1)
            collateral_price: 1,
        }
    }
}

impl Oracle for SimOracle {
    fn validate_and_get_prices(&self, market_id: MarketId) -> Result<OraclePrices, String> {
        let symbol = self
            .market_symbols
            .get(&market_id)
            .ok_or_else(|| format!("unknown_market_id:{:?}", market_id))?;

        let cache = self.cache.borrow();
        let (min, max) = cache
            .get(symbol)
            .ok_or_else(|| format!("no_price_for_symbol:{}", symbol))?;

        Ok(OraclePrices {
            index_price_min: min,
            index_price_max: max,
            collateral_price_min: self.collateral_price,
            collateral_price_max: self.collateral_price,
        })
    }
}

// ==== ExchangeAgent with perp-futures Executor ====

pub struct ExchangeAgent {
    id: AgentId,
    name: String,
    markets: Vec<MarketConfig>,
    last_prices: HashMap<String, u64>,

    executor: Executor<BasicServicesBundle, SimOracle>,
    price_cache: Rc<RefCell<PriceCache>>,

    accounts: HashMap<AgentId, AccountId>,
    next_account_idx: u32,

    /// Maps symbol -> (market_id, collateral_asset)
    symbol_to_market: HashMap<String, (MarketId, AssetId)>,
}

impl ExchangeAgent {
    pub fn new(id: AgentId, name: String, markets: Vec<MarketConfig>) -> Self {
        let price_cache = Rc::new(RefCell::new(PriceCache::new()));

        let mut state = State::default();
        let mut symbol_to_market = HashMap::new();

        // Setup each market from config
        for (idx, market_cfg) in markets.iter().enumerate() {
            let market_id = MarketId(market_cfg.id);
            let collateral_asset = AssetId(idx as u32 * 2); // USDT
            let index_asset = AssetId(idx as u32 * 2 + 1); // ETH/BTC/etc

            // Add initial liquidity
            state
                .pool_balances
                .add_liquidity(market_id, collateral_asset, market_cfg.collateral_amount);
            state
                .pool_balances
                .add_liquidity(market_id, index_asset, market_cfg.index_amount);

            // Configure market state
            {
                let market = state.markets.entry(market_id).or_default();
                market.id = market_id;
                market.index_token = index_asset;
                market.long_asset = index_asset;
                market.short_asset = collateral_asset;
                market.liquidity_usd = market_cfg.liquidity_usd;
            }

            symbol_to_market.insert(market_cfg.symbol.clone(), (market_id, collateral_asset));

            println!(
                "[Exchange {}] Market {} ({}) initialized: liquidity=${:.0}M",
                name,
                market_cfg.symbol,
                market_cfg.id,
                market_cfg.liquidity_usd as f64 / 1_000_000_000_000.0
            );
        }

        let services = BasicServicesBundle::default();
        let oracle = SimOracle::new(price_cache.clone(), &markets);
        let executor = Executor::new(state, services, oracle);

        Self {
            id,
            name,
            markets,
            last_prices: HashMap::new(),
            executor,
            price_cache,
            accounts: HashMap::new(),
            next_account_idx: 0,
            symbol_to_market,
        }
    }

    fn get_or_create_account(&mut self, agent_id: AgentId) -> AccountId {
        *self.accounts.entry(agent_id).or_insert_with(|| {
            let idx = self.next_account_idx;
            self.next_account_idx += 1;
            let mut bytes = [0u8; 32];
            bytes[0..4].copy_from_slice(&idx.to_le_bytes());
            AccountId(bytes)
        })
    }

    fn convert_side(side: SimSide) -> Side {
        match side {
            SimSide::Buy => Side::Long,
            SimSide::Sell => Side::Short,
        }
    }

    fn process_close_order(&mut self, sim: &mut dyn SimulatorApi, from: AgentId, order: &CloseOrderPayload, now_ns: u64) {
        let (market_id, collateral_asset) = match self.symbol_to_market.get(&order.symbol) {
            Some(m) => *m,
            None => {
                println!(
                    "[Exchange {}] CLOSE REJECTED from {}: unknown symbol {}",
                    self.name, from, order.symbol
                );
                return;
            }
        };

        let account = self.get_or_create_account(from);
        let side = Self::convert_side(order.side);

        // Find the position
        let position_key = PositionKey {
            account,
            market_id,
            collateral_token: collateral_asset,
            side,
        };

        let position = match self.executor.state.positions.get(&position_key) {
            Some(p) => p.clone(),
            None => {
                println!(
                    "[Exchange {}] CLOSE REJECTED from {}: no {:?} position for {}",
                    self.name, from, order.side, order.symbol
                );
                return;
            }
        };

        let now: Timestamp = now_ns / 1_000_000_000;
        let execution_price = self.last_prices.get(&order.symbol).copied().unwrap_or(0);

        // Create decrease order for full position size
        // Note: withdraw_collateral_amount = 0 lets the executor calculate the correct payout
        // after accounting for PnL, fees, etc.
        let perp_order = Order {
            account,
            market_id,
            collateral_token: collateral_asset,
            side,
            order_type: OrderType::Decrease,
            collateral_delta_tokens: 0,
            size_delta_usd: position.size_usd, // Close full position
            withdraw_collateral_amount: 0,     // Executor will calculate payout
            target_leverage_x: 0,
            created_at: now,
            valid_from: now,
            valid_until: now + 3600,
        };

        let order_id = self.executor.submit_order(perp_order);

        match self.executor.execute_order(now, order_id) {
            Ok(()) => {
                println!(
                    "[Exchange {}] CLOSED {} from={} side={:?} size=${:.2}",
                    self.name,
                    order.symbol,
                    from,
                    order.side,
                    position.size_usd as f64 / 1_000_000.0
                );

                // Emit execution event
                sim.emit_event(SimEvent::OrderExecuted {
                    ts: now_ns,
                    account: from,
                    symbol: order.symbol.clone(),
                    side: order.side,
                    size_usd: position.size_usd as u64,
                    collateral: position.collateral_amount as u64,
                    execution_price,
                    leverage: 0, // N/A for close
                    order_type: "Decrease".to_string(),
                });

                if let Some(market) = self.executor.state.markets.get(&market_id) {
                    println!(
                        "[Exchange {}] {} OI: long=${:.2} short=${:.2}",
                        self.name,
                        order.symbol,
                        market.oi_long_usd as f64 / 1_000_000.0,
                        market.oi_short_usd as f64 / 1_000_000.0
                    );
                }
            }
            Err(e) => {
                println!(
                    "[Exchange {}] CLOSE REJECTED {} from={} error={}",
                    self.name, order.symbol, from, e
                );
            }
        }
    }

    fn process_market_order(&mut self, sim: &mut dyn SimulatorApi, from: AgentId, order: &MarketOrderPayload, now_ns: u64) {
        let (market_id, collateral_asset) = match self.symbol_to_market.get(&order.symbol) {
            Some(m) => *m,
            None => {
                println!(
                    "[Exchange {}] REJECTED from {}: unknown symbol {}",
                    self.name, from, order.symbol
                );
                return;
            }
        };

        let account = self.get_or_create_account(from);
        let side = Self::convert_side(order.side);

        let price = match self.last_prices.get(&order.symbol) {
            Some(p) => *p as Usd,
            None => {
                println!(
                    "[Exchange {}] REJECTED from {}: no price for {}",
                    self.name, from, order.symbol
                );
                return;
            }
        };

        // qty * price = size in USD
        let leverage = order.leverage.max(1) as Usd; // minimum 1x
        let size_delta_usd: Usd = (order.qty as Usd) * price;
        let collateral_delta: TokenAmount = size_delta_usd / leverage;
        let now: Timestamp = now_ns / 1_000_000_000;

        let perp_order = Order {
            account,
            market_id,
            collateral_token: collateral_asset,
            side,
            order_type: OrderType::Increase,
            collateral_delta_tokens: collateral_delta,
            size_delta_usd,
            withdraw_collateral_amount: 0,
            target_leverage_x: order.leverage as i64,
            created_at: now,
            valid_from: now,
            valid_until: now + 3600,
        };

        let order_id = self.executor.submit_order(perp_order);

        match self.executor.execute_order(now, order_id) {
            Ok(()) => {
                println!(
                    "[Exchange {}] EXECUTED {} from={} side={:?} qty={} size=${:.2} collateral=${:.2} leverage={}x",
                    self.name,
                    order.symbol,
                    from,
                    order.side,
                    order.qty,
                    size_delta_usd as f64 / 1_000_000.0,
                    collateral_delta as f64 / 1_000_000.0,
                    order.leverage
                );

                // Emit execution event
                sim.emit_event(SimEvent::OrderExecuted {
                    ts: now_ns,
                    account: from,
                    symbol: order.symbol.clone(),
                    side: order.side,
                    size_usd: size_delta_usd as u64,
                    collateral: collateral_delta as u64,
                    execution_price: price as u64,
                    leverage: order.leverage,
                    order_type: "Increase".to_string(),
                });

                if let Some(market) = self.executor.state.markets.get(&market_id) {
                    println!(
                        "[Exchange {}] {} OI: long=${:.2} short=${:.2}",
                        self.name,
                        order.symbol,
                        market.oi_long_usd as f64 / 1_000_000.0,
                        market.oi_short_usd as f64 / 1_000_000.0
                    );
                }
            }
            Err(e) => {
                println!(
                    "[Exchange {}] REJECTED {} from={} error={}",
                    self.name, order.symbol, from, e
                );
            }
        }
    }
}

impl Agent for ExchangeAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn on_start(&mut self, _sim: &mut dyn SimulatorApi) {
        println!("[Exchange {}] started with {} market(s)", self.name, self.markets.len());
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        let pos_count = self.executor.state.positions.iter().count();
        println!("[Exchange {}] === FINAL STATE ===", self.name);

        for market_cfg in &self.markets {
            let market_id = MarketId(market_cfg.id);
            if let Some(market) = self.executor.state.markets.get(&market_id) {
                println!(
                    "[Exchange {}] {} OI: long=${:.2} short=${:.2}",
                    self.name,
                    market_cfg.symbol,
                    market.oi_long_usd as f64 / 1_000_000.0,
                    market.oi_short_usd as f64 / 1_000_000.0
                );
            }
        }

        println!("[Exchange {}] Total positions: {}", self.name, pos_count);
    }

    fn on_message(&mut self, sim: &mut dyn SimulatorApi, msg: &Message) {
        match msg.msg_type {
            MessageType::OracleTick => {
                if let MessagePayload::OracleTick(OracleTickPayload {
                    symbol,
                    price,
                    publish_time: _,
                    signature: _,
                }) = &msg.payload
                {
                    // Check if this symbol is one of our markets
                    if self.symbol_to_market.contains_key(symbol) {
                        self.price_cache.borrow_mut().update(symbol, price.min, price.max);

                        let mid_price = (price.min + price.max) / 2;
                        self.last_prices.insert(symbol.clone(), mid_price);

                        println!(
                            "[Exchange {}] PRICE {} = ${:.2}",
                            self.name,
                            symbol,
                            mid_price as f64 / 1_000_000.0
                        );
                    }
                }
            }

            MessageType::MarketOrder => {
                if let MessagePayload::MarketOrder(order) = &msg.payload {
                    let now_ns = sim.now_ns();
                    self.process_market_order(sim, msg.from, order, now_ns);
                }
            }

            MessageType::CloseOrder => {
                if let MessagePayload::CloseOrder(order) = &msg.payload {
                    let now_ns = sim.now_ns();
                    self.process_close_order(sim, msg.from, order, now_ns);
                }
            }

            MessageType::LimitOrder => {
                println!(
                    "[Exchange {}] LIMIT_ORDER from {} (not implemented)",
                    self.name, msg.from
                );
            }

            _ => {}
        }
    }
}
