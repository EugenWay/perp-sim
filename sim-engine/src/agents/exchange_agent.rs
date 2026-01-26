use crate::agents::Agent;
use crate::events::SimEvent;
use crate::messages::{
    AgentId, CloseOrderPayload, ExecutionType, KeeperRewardPayload, MarketOrderPayload, MarketStatePayload, Message,
    MessagePayload, MessageType, OracleTickPayload, OrderExecutedPayload, OrderExecutionType, OrderId, OrderPayload,
    OrderType as SimOrderType, PendingOrderInfo, PendingOrdersListPayload, PositionLiquidatedPayload,
    PreviewRequestPayload, PreviewResponsePayload, Price, Side as SimSide, SimulatorApi,
};
use crate::pending_orders::{PendingOrder, PendingOrderStore};
use crate::trigger_checker;
use perp_futures::executor::Executor;
use perp_futures::oracle::Oracle;
use perp_futures::services::BasicServicesBundle;
use perp_futures::state::PositionKey;
use perp_futures::state::State;
use perp_futures::types::{
    AccountId, AssetId, MarketId, OraclePrices, Order, OrderType, Side, SignedU256, Timestamp, Usd,
};
use primitive_types::U256;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

// ==== Price Normalization ====
// perp-futures uses USD(1e30) per 1 atom of token
// Our oracle provides prices in micro-USD (1e6 = $1)
//
// Conversion formula:
//   price_per_atom = price_micro_usd * 10^(24 - index_decimals)
//
// Examples:
//   ETH ($3000, 18 decimals): 3000_000_000 * 10^6 = 3000 * 10^12
//   BTC ($100000, 8 decimals): 100000_000_000 * 10^16 = 100000 * 10^22

/// Convert micro-USD price to USD(1e30) per atom
/// micro_usd: price in micro-USD (1e6 = $1)
/// index_decimals: token decimals (ETH=18, BTC=8)
fn normalize_price_to_atom(micro_usd: u64, index_decimals: u32) -> U256 {
    // price_per_atom = micro_usd * 10^(24 - index_decimals)
    // = micro_usd * 10^24 / 10^index_decimals
    let exp = 24u32.saturating_sub(index_decimals);
    U256::from(micro_usd) * U256::exp10(exp as usize)
}

/// Convert USD(1e30) per atom price back to micro-USD for display
fn denormalize_price_from_atom(price_atom: U256, index_decimals: u32) -> u64 {
    let exp = 24u32.saturating_sub(index_decimals);
    let divisor = U256::exp10(exp as usize);
    if divisor.is_zero() {
        return 0;
    }
    (price_atom / divisor).low_u64()
}

/// Convert SignedU256 PnL from USD(1e30) to micro-USD for display
fn pnl_to_micro_usd(pnl: SignedU256) -> i64 {
    // USD(1e30) -> micro-USD: divide by 10^24
    let divisor = U256::exp10(24);
    let mag_micro = (pnl.mag / divisor).low_u64();
    if pnl.is_negative {
        -(mag_micro as i64)
    } else {
        mag_micro as i64
    }
}

/// Convert USD(1e30) to micro-USD for display  
fn usd_to_micro(usd: U256) -> u64 {
    // USD(1e30) -> micro-USD (1e6): divide by 10^24
    // BUT: size_usd in positions is absolute USD(1e30), so divide by 10^24 for display
    let divisor = U256::exp10(24);
    (usd / divisor).low_u64()
}

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
    pub index_decimals: u32,      // Token decimals (ETH=18, BTC=8)
    pub collateral_decimals: u32, // Collateral decimals (USDT=6)
}

// ==== SimOracle: adapter for perp-futures Oracle trait ====

#[derive(Clone)]
struct PriceCache {
    /// Maps symbol -> (index_price_min, index_price_max) in USD(1e30) per atom
    prices: HashMap<String, (Usd, Usd)>,
    /// Maps symbol -> index_decimals (needed for price normalization)
    decimals: HashMap<String, u32>,
}

impl PriceCache {
    fn new() -> Self {
        Self {
            prices: HashMap::new(),
            decimals: HashMap::new(),
        }
    }

    fn set_decimals(&mut self, symbol: &str, index_decimals: u32) {
        self.decimals.insert(symbol.to_string(), index_decimals);
    }

    /// Update price from micro-USD (1e6 = $1) to USD(1e30) per atom
    fn update(&mut self, symbol: &str, min_micro: u64, max_micro: u64) {
        let decimals = self.decimals.get(symbol).copied().unwrap_or(18);
        let min_atom = normalize_price_to_atom(min_micro, decimals);
        let max_atom = normalize_price_to_atom(max_micro, decimals);
        self.prices.insert(symbol.to_string(), (min_atom, max_atom));
    }

    fn get(&self, symbol: &str) -> Option<(Usd, Usd)> {
        self.prices.get(symbol).copied()
    }

    fn get_decimals(&self, symbol: &str) -> u32 {
        self.decimals.get(symbol).copied().unwrap_or(18)
    }
}

/// Oracle implementation that reads prices from a shared cache
pub struct SimOracle {
    cache: Rc<RefCell<PriceCache>>,
    market_symbols: HashMap<MarketId, String>,
    /// Collateral price in USD(1e30) per atom
    /// For USDC (6 decimals, $1): 1 * 10^30 / 10^6 = 10^24
    collateral_price: Usd,
}

impl SimOracle {
    fn new(cache: Rc<RefCell<PriceCache>>, markets: &[MarketConfig]) -> Self {
        let mut market_symbols = HashMap::new();
        for m in markets {
            market_symbols.insert(MarketId(m.id), m.symbol.clone());
        }

        // Collateral (USDC-like, 6 decimals) at $1 per token
        // USD(1e30) per atom = 1 * 10^30 / 10^6 = 10^24
        let collateral_price = U256::exp10(24);

        Self {
            cache,
            market_symbols,
            collateral_price,
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

    symbol_to_market: HashMap<String, (MarketId, AssetId)>,
    #[allow(dead_code)]
    symbol_decimals: HashMap<String, (u32, u32)>,

    pending_orders: PendingOrderStore,
}

impl ExchangeAgent {
    pub fn new(id: AgentId, name: String, markets: Vec<MarketConfig>) -> Self {
        let price_cache = Rc::new(RefCell::new(PriceCache::new()));

        let mut state = State::default();
        let mut symbol_to_market = HashMap::new();
        let mut symbol_decimals = HashMap::new();

        // Setup each market from config
        for (idx, market_cfg) in markets.iter().enumerate() {
            let market_id = MarketId(market_cfg.id);
            let collateral_asset = AssetId(idx as u32 * 2); // USDT
            let index_asset = AssetId(idx as u32 * 2 + 1); // ETH/BTC/etc

            // Register decimals for price normalization
            price_cache
                .borrow_mut()
                .set_decimals(&market_cfg.symbol, market_cfg.index_decimals);

            // Add initial liquidity (in atoms)
            // Collateral: convert from micro-USD to atoms (collateral_decimals)
            let collateral_atoms = U256::from(market_cfg.collateral_amount as u128)
                * U256::exp10(market_cfg.collateral_decimals as usize)
                / U256::exp10(6); // micro-USD to atoms
            state
                .pool_balances
                .add_liquidity(market_id, collateral_asset, collateral_atoms);

            // Index tokens: as provided (already in atoms or conceptual units)
            state
                .pool_balances
                .add_liquidity(market_id, index_asset, U256::from(market_cfg.index_amount as u128));

            // Configure market state
            // liquidity_usd in USD(1e30): convert from micro-USD
            let liquidity_usd_1e30 = U256::from(market_cfg.liquidity_usd as u128) * U256::exp10(24);
            {
                let market = state.markets.entry(market_id).or_default();
                market.id = market_id;
                market.index_token = index_asset;
                market.long_asset = index_asset;
                market.short_asset = collateral_asset;
                market.liquidity_usd = liquidity_usd_1e30;
            }

            symbol_to_market.insert(market_cfg.symbol.clone(), (market_id, collateral_asset));
            symbol_decimals.insert(
                market_cfg.symbol.clone(),
                (market_cfg.index_decimals, market_cfg.collateral_decimals),
            );

            println!(
                "[Exchange {}] Market {} ({}) initialized: liquidity=${:.0}M, index_decimals={}, collateral_decimals={}",
                name,
                market_cfg.symbol,
                market_cfg.id,
                market_cfg.liquidity_usd as f64 / 1_000_000_000_000.0,
                market_cfg.index_decimals,
                market_cfg.collateral_decimals
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
            symbol_decimals,
            pending_orders: PendingOrderStore::new(),
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

    fn convert_side_back(side: Side) -> SimSide {
        match side {
            Side::Long => SimSide::Buy,
            Side::Short => SimSide::Sell,
        }
    }

    /// Broadcast MarketState to all agents (for OI-based strategies)
    fn broadcast_market_state(&self, sim: &mut dyn SimulatorApi) {
        for market_cfg in &self.markets {
            let market_id = MarketId(market_cfg.id);
            if let Some(market) = self.executor.state.markets.get(&market_id) {
                let payload = MessagePayload::MarketState(MarketStatePayload {
                    symbol: market_cfg.symbol.clone(),
                    oi_long_usd: usd_to_micro(market.oi_long_usd) as i128,
                    oi_short_usd: usd_to_micro(market.oi_short_usd) as i128,
                    liquidity_usd: usd_to_micro(market.liquidity_usd) as i128,
                });
                sim.broadcast(self.id, MessageType::MarketState, payload);
            }
        }
    }

    /// Emit market snapshots only (OI/liquidity) for UI updates
    fn emit_market_snapshots_only(&self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        for market_cfg in &self.markets {
            let market_id = MarketId(market_cfg.id);
            if let Some(market) = self.executor.state.markets.get(&market_id) {
                sim.emit_event(SimEvent::MarketSnapshot {
                    ts: now_ns,
                    symbol: market_cfg.symbol.clone(),
                    oi_long_usd: usd_to_micro(market.oi_long_usd),
                    oi_short_usd: usd_to_micro(market.oi_short_usd),
                    liquidity_usd: usd_to_micro(market.liquidity_usd),
                });
            }
        }
    }

    /// Emit snapshots for all positions and market state
    /// Called on each oracle tick to track PnL evolution
    /// Returns list of (agent_id, symbol, side) for liquidatable positions
    fn emit_snapshots(&self, sim: &mut dyn SimulatorApi, now_ns: u64) -> Vec<(AgentId, String, Side)> {
        // Reverse lookup: account_id -> agent_id
        let account_to_agent: HashMap<AccountId, AgentId> =
            self.accounts.iter().map(|(agent, acc)| (*acc, *agent)).collect();

        let now_sec = (now_ns / 1_000_000_000) as Timestamp;
        let mut to_liquidate = Vec::new();

        // Emit position snapshots
        for (key, position) in self.executor.state.positions.iter() {
            // Find symbol for this market
            let symbol = self
                .symbol_to_market
                .iter()
                .find(|(_, (mid, _))| *mid == key.market_id)
                .map(|(s, _)| s.clone())
                .unwrap_or_else(|| format!("UNKNOWN-{:?}", key.market_id));

            let index_decimals = self.price_cache.borrow().get_decimals(&symbol);
            let current_price_micro = self.last_prices.get(&symbol).copied().unwrap_or(0);

            // Calculate entry price: size_usd / size_tokens
            // Both are in USD(1e30), result is price per atom
            let entry_price_atom = if !position.size_tokens.is_zero() {
                position.size_usd / position.size_tokens
            } else {
                U256::zero()
            };
            let entry_price_micro = denormalize_price_from_atom(entry_price_atom, index_decimals);

            // Calculate leverage: size_usd / (collateral_amount * collateral_price)
            // collateral_price = 10^24 (for USDC at $1)
            let collateral_usd = position.collateral_amount * U256::exp10(24);
            let leverage = if !collateral_usd.is_zero() {
                (position.size_usd / collateral_usd).low_u32().max(1)
            } else {
                1
            };

            // Use engine API for liquidation check and price
            let (liquidatable, liq_price_micro, pnl_micro) =
                match self.executor.is_liquidatable_by_margin(now_sec, *key) {
                    Ok(preview) => {
                        let liq_price = self
                            .executor
                            .calculate_liquidation_price(now_sec, *key)
                            .unwrap_or(U256::zero());
                        let liq_price_micro = denormalize_price_from_atom(liq_price, index_decimals);
                        let pnl_micro = pnl_to_micro_usd(preview.pnl_usd);
                        (preview.is_liquidatable, liq_price_micro, pnl_micro)
                    }
                    Err(_) => (false, 0u64, 0i64),
                };

            // Get agent_id from account
            let agent_id = account_to_agent.get(&key.account).copied().unwrap_or(0);

            // Convert USD(1e30) values to micro-USD for display
            let size_usd_micro = usd_to_micro(position.size_usd);
            let collateral_micro = (position.collateral_amount * U256::exp10(24) / U256::exp10(24)).low_u64(); // atoms to micro (simplified for $1 collateral)

            sim.emit_event(SimEvent::PositionSnapshot {
                ts: now_ns,
                account: agent_id,
                symbol: symbol.clone(),
                side: Self::convert_side_back(key.side),
                size_usd: size_usd_micro,
                size_tokens: position.size_tokens.low_u128() as i128,
                collateral: collateral_micro,
                entry_price: entry_price_micro,
                current_price: current_price_micro,
                unrealized_pnl: pnl_micro,
                liquidation_price: liq_price_micro,
                leverage_actual: leverage,
                is_liquidatable: liquidatable,
                opened_at_sec: position.opened_at,
            });

            // Collect liquidatable positions (with grace period)
            // Grace period: don't liquidate positions opened less than 10 seconds ago
            const LIQUIDATION_GRACE_PERIOD_SEC: u64 = 10;
            let position_age_sec = now_sec.saturating_sub(position.opened_at);
            let past_grace_period = position_age_sec >= LIQUIDATION_GRACE_PERIOD_SEC;

            if liquidatable && agent_id != 0 && past_grace_period {
                to_liquidate.push((agent_id, symbol.clone(), key.side));
                println!(
                    "[Exchange {}] ⚠️ LIQUIDATABLE: {} {:?} agent={} size=${:.2} pnl=${:.2}",
                    self.name,
                    symbol,
                    key.side,
                    agent_id,
                    size_usd_micro as f64 / 1_000_000.0,
                    pnl_micro as f64 / 1_000_000.0,
                );
            } else if liquidatable && agent_id != 0 && !past_grace_period {
                println!(
                    "[Exchange {}] ⏳ GRACE PERIOD: {} {:?} agent={} ({}s remaining)",
                    self.name,
                    symbol,
                    key.side,
                    agent_id,
                    LIQUIDATION_GRACE_PERIOD_SEC - position_age_sec,
                );
            }
        }

        // Emit market snapshots
        self.emit_market_snapshots_only(sim, now_ns);

        to_liquidate
    }

    /// Send liquidation notification to trader
    fn notify_liquidation(
        &self,
        sim: &mut dyn SimulatorApi,
        agent_id: AgentId,
        symbol: String,
        side: Side,
        size_usd_micro: i64,
        pnl_micro: i64,
        collateral_micro: i64,
    ) {
        let sim_side = Self::convert_side_back(side);
        let payload = MessagePayload::PositionLiquidated(PositionLiquidatedPayload {
            symbol,
            side: sim_side,
            size_usd: size_usd_micro as i128,
            pnl: pnl_micro as i128,
            collateral_lost: collateral_micro as i128,
        });
        sim.send(self.id, agent_id, MessageType::PositionLiquidated, payload);
    }

    /// Execute liquidation for a position
    /// This actually closes the position using the perp-futures engine
    fn execute_liquidation(
        &mut self,
        sim: &mut dyn SimulatorApi,
        agent_id: AgentId,
        symbol: String,
        side: Side,
        now_ns: u64,
    ) -> Result<(), String> {
        let now = (now_ns / 1_000_000_000) as Timestamp;

        // Get account for this agent
        let account = match self.accounts.get(&agent_id) {
            Some(acc) => *acc,
            None => return Err(format!("Agent {} has no account", agent_id)),
        };

        // Get market info
        let (market_id, collateral_asset) = match self.symbol_to_market.get(&symbol) {
            Some((mid, cid)) => (*mid, *cid),
            None => return Err(format!("Unknown symbol: {}", symbol)),
        };

        // Get position key
        let key = PositionKey {
            account,
            market_id,
            collateral_token: collateral_asset,
            side,
        };

        // Get position to liquidate
        let position = match self.executor.state.positions.get(&key) {
            Some(pos) => pos.clone(),
            None => return Err("No position found for liquidation".into()),
        };

        // Get detailed liquidation info from engine
        let (pnl_micro, _liquidation_preview) = match self.executor.is_liquidatable_by_margin(now, key) {
            Ok(preview) => {
                let pnl = pnl_to_micro_usd(preview.pnl_usd);
                println!(
                    "[Exchange {}] 🔍 LIQUIDATION PREVIEW for {} agent={} side={:?}:",
                    self.name, symbol, agent_id, side
                );
                println!("  ├─ RAW PNL from engine: {:?}", preview.pnl_usd);
                println!("  ├─ PNL in micro-USD: ${:.2}", pnl as f64 / 1_000_000.0);
                println!("  ├─ is_liquidatable: {}", preview.is_liquidatable);
                println!(
                    "  ├─ collateral_value_usd: ${:.2}",
                    usd_to_micro(preview.collateral_value_usd) as f64 / 1_000_000.0
                );
                println!(
                    "  ├─ price_impact_usd: ${:.2}",
                    pnl_to_micro_usd(preview.price_impact_usd) as f64 / 1_000_000.0
                );
                println!(
                    "  ├─ borrowing_fee_usd: ${:.2}",
                    usd_to_micro(preview.borrowing_fee_usd) as f64 / 1_000_000.0
                );
                println!(
                    "  ├─ funding_fee_usd: ${:.2}",
                    pnl_to_micro_usd(preview.funding_fee_usd) as f64 / 1_000_000.0
                );
                println!(
                    "  ├─ close_fees_usd: ${:.2}",
                    usd_to_micro(preview.close_fees_usd) as f64 / 1_000_000.0
                );
                println!(
                    "  ├─ equity_usd: ${:.2}",
                    pnl_to_micro_usd(preview.equity_usd) as f64 / 1_000_000.0
                );
                println!(
                    "  └─ required_usd: ${:.2}",
                    usd_to_micro(preview.required_usd) as f64 / 1_000_000.0
                );
                (pnl, Some(preview))
            }
            Err(e) => {
                println!(
                    "[Exchange {}] ⚠️  Could not get liquidation preview: {:?}",
                    self.name, e
                );
                (0, None)
            }
        };

        let size_usd_micro = usd_to_micro(position.size_usd) as i64;
        let collateral_micro = position.collateral_amount.low_u64() as i64;
        let current_price_micro = self.last_prices.get(&symbol).copied().unwrap_or(0);

        // Get liquidation price from engine
        let liq_price_micro = match self.executor.calculate_liquidation_price(now, key) {
            Ok(liq_price_atom) => {
                let (index_decimals, _collateral_decimals) =
                    self.symbol_decimals.get(&symbol).copied().unwrap_or((18, 6));
                denormalize_price_from_atom(liq_price_atom, index_decimals)
            }
            Err(_) => 0,
        };

        // Calculate leverage (size_usd in USD(1e30) / collateral_amount in atoms)
        // For USDC (6 decimals), 1 atom = 1 micro-USD
        // So collateral in USD = collateral_amount * 1e-6 (in actual USD)
        // size_usd is in USD(1e30), so size in actual USD = size_usd / 1e30
        // leverage = (size_usd / 1e30) / (collateral_amount * 1e-6)
        //          = size_usd / (collateral_amount * 1e24)
        let leverage_calc = if !position.collateral_amount.is_zero() {
            let denominator = position.collateral_amount * U256::exp10(24);
            if !denominator.is_zero() {
                (position.size_usd / denominator).low_u64()
            } else {
                0
            }
        } else {
            0
        };

        println!(
            "[Exchange {}] 🔥 EXECUTING LIQUIDATION: {} {:?} agent={}",
            self.name, symbol, side, agent_id
        );
        println!(
            "  ├─ Position size: ${:.2} (raw: {})",
            size_usd_micro as f64 / 1_000_000.0,
            position.size_usd
        );
        println!(
            "  ├─ Collateral: ${:.2} (raw: {} atoms)",
            collateral_micro as f64 / 1_000_000.0,
            position.collateral_amount
        );
        println!("  ├─ PnL: ${:.2}", pnl_micro as f64 / 1_000_000.0);
        println!("  ├─ Current price: ${:.2}", current_price_micro as f64 / 1_000_000.0);
        println!("  ├─ Liquidation price: ${:.2}", liq_price_micro as f64 / 1_000_000.0);
        println!("  └─ Calculated Leverage: {}x", leverage_calc);

        // Create liquidation order using OrderType::Liquidation
        let perp_order = Order {
            account,
            market_id,
            collateral_token: collateral_asset,
            side,
            order_type: OrderType::Liquidation,
            collateral_delta_tokens: U256::zero(),
            size_delta_usd: position.size_usd, // Close full position
            withdraw_collateral_amount: U256::zero(),
            target_leverage_x: 0,
            created_at: now,
            valid_from: now,
            valid_until: now + 3600,
        };

        let order_id = self.executor.submit_order(perp_order);

        match self.executor.execute_order(now, order_id) {
            Ok(()) => {
                println!(
                    "[Exchange {}] ✅ LIQUIDATION EXECUTED: {} agent={} side={:?}",
                    self.name, symbol, agent_id, side
                );

                // Emit liquidation execution event
                sim.emit_event(SimEvent::OrderExecuted {
                    ts: now_ns,
                    account: agent_id,
                    symbol: symbol.clone(),
                    side: Self::convert_side_back(side),
                    size_usd: size_usd_micro as u64,
                    collateral: collateral_micro as u64,
                    execution_price: current_price_micro,
                    leverage: 0,
                    order_type: "Liquidation".to_string(),
                    pnl: pnl_micro,
                });

                // Emit specific liquidation event for detailed analytics
                sim.emit_event(SimEvent::PositionLiquidated {
                    ts: now_ns,
                    account: agent_id,
                    symbol: symbol.clone(),
                    side: Self::convert_side_back(side),
                    size_usd: size_usd_micro as u64,
                    collateral_lost: collateral_micro as u64,
                    pnl: pnl_micro,
                    liquidation_price: current_price_micro,
                });

                // Send notification to trader
                self.notify_liquidation(sim, agent_id, symbol, side, size_usd_micro, pnl_micro, collateral_micro);

                // Emit fresh market snapshot for UI updates
                self.emit_market_snapshots_only(sim, now_ns);
                // Broadcast updated market state after liquidation
                self.broadcast_market_state(sim);

                Ok(())
            }
            Err(e) => {
                let err_msg = format!("Liquidation execution failed: {:?}", e);
                println!("[Exchange {}] ❌ {}", self.name, err_msg);
                Err(err_msg)
            }
        }
    }

    /// Scan for liquidatable positions and execute liquidations
    fn process_liquidation_scan(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        // Get list of positions to liquidate
        let to_liquidate = self.get_liquidatable_positions(now_ns);

        if to_liquidate.is_empty() {
            return;
        }

        println!(
            "[Exchange {}] 🔍 Found {} positions to liquidate",
            self.name,
            to_liquidate.len()
        );

        // Execute liquidation for each underwater position
        for (agent_id, symbol, side) in to_liquidate {
            match self.execute_liquidation(sim, agent_id, symbol.clone(), side, now_ns) {
                Ok(()) => {
                    println!(
                        "[Exchange {}] Liquidated: agent={} {} {:?}",
                        self.name, agent_id, symbol, side
                    );
                }
                Err(e) => {
                    eprintln!(
                        "[Exchange {}] Failed to liquidate agent={} {} {:?}: {}",
                        self.name, agent_id, symbol, side, e
                    );
                }
            }
        }
    }

    /// Get list of liquidatable positions using engine API
    /// Returns list of (agent_id, symbol, side)
    fn get_liquidatable_positions(&self, now_ns: u64) -> Vec<(AgentId, String, Side)> {
        let now_sec = (now_ns / 1_000_000_000) as Timestamp;

        // Reverse lookup: account_id -> agent_id
        let account_to_agent: HashMap<AccountId, AgentId> =
            self.accounts.iter().map(|(agent, acc)| (*acc, *agent)).collect();

        let mut to_liquidate = Vec::new();

        // Grace period: don't liquidate positions opened less than 10 seconds ago
        const LIQUIDATION_GRACE_PERIOD_SEC: u64 = 10;

        // Check all positions using engine API
        for (key, position) in self.executor.state.positions.iter() {
            // Find symbol for this market
            let symbol = self
                .symbol_to_market
                .iter()
                .find(|(_, (mid, _))| *mid == key.market_id)
                .map(|(s, _)| s.clone())
                .unwrap_or_else(|| format!("UNKNOWN-{:?}", key.market_id));

            // Check grace period
            let position_age_sec = now_sec.saturating_sub(position.opened_at);
            if position_age_sec < LIQUIDATION_GRACE_PERIOD_SEC {
                continue; // Skip - still in grace period
            }

            // Use engine API to check if liquidatable
            let liquidatable = match self.executor.is_liquidatable_by_margin(now_sec, *key) {
                Ok(preview) => preview.is_liquidatable,
                Err(_) => false,
            };

            // Get agent_id from account
            let agent_id = account_to_agent.get(&key.account).copied().unwrap_or(0);

            // Collect liquidatable positions (excluding exchange itself)
            if liquidatable && agent_id != 0 {
                to_liquidate.push((agent_id, symbol, key.side));
            }
        }

        to_liquidate
    }

    fn validate_order(&self, order: &OrderPayload) -> Result<(), String> {
        if !self.symbol_to_market.contains_key(&order.symbol) {
            return Err(format!("unknown symbol: {}", order.symbol));
        }

        match order.execution_type {
            ExecutionType::Market => {}
            ExecutionType::Limit | ExecutionType::StopLoss | ExecutionType::TakeProfit => {
                if order.trigger_price.is_none() {
                    return Err("trigger_price required for conditional orders".into());
                }
            }
        }

        match order.order_type {
            SimOrderType::Increase => {
                if order.qty.is_none() || order.qty.unwrap() <= 0.0 {
                    return Err("qty required for Increase".into());
                }
            }
            SimOrderType::Decrease => {}
        }

        // SL/TP only for Decrease
        if matches!(
            order.execution_type,
            ExecutionType::StopLoss | ExecutionType::TakeProfit
        ) && order.order_type != SimOrderType::Decrease
        {
            return Err("StopLoss/TakeProfit only valid for Decrease orders".into());
        }

        Ok(())
    }

    fn process_submit_order(&mut self, sim: &mut dyn SimulatorApi, from: AgentId, order: &OrderPayload, now_ns: u64) {
        if let Err(e) = self.validate_order(order) {
            println!("[Exchange {}] REJECTED from {}: {}", self.name, from, e);
            return;
        }

        // Market orders execute immediately
        if order.execution_type == ExecutionType::Market {
            match order.order_type {
                SimOrderType::Increase => {
                    let market_order = MarketOrderPayload {
                        symbol: order.symbol.clone(),
                        side: order.side,
                        qty: order.qty.unwrap_or(0.0),
                        leverage: order.leverage.unwrap_or(5),
                    };
                    self.process_market_order(sim, from, &market_order, now_ns);
                }
                SimOrderType::Decrease => {
                    let close_order = CloseOrderPayload {
                        symbol: order.symbol.clone(),
                        side: order.side,
                    };
                    self.process_close_order(sim, from, &close_order, now_ns);
                }
            }
            return;
        }

        // Add to pending orders
        let order_id = self.pending_orders.insert(from, order.clone(), now_ns);

        println!(
            "[Exchange {}] PENDING #{} from={} {:?} {:?} trigger=${:.2}",
            self.name,
            order_id,
            from,
            order.execution_type,
            order.side,
            order.trigger_price.unwrap_or(0) as f64 / 1_000_000.0
        );

        sim.send(
            self.id,
            from,
            MessageType::OrderPending,
            MessagePayload::Text(format!("order_id:{}", order_id)),
        );
    }

    fn process_cancel_order(&mut self, _sim: &mut dyn SimulatorApi, from: AgentId, order_id: OrderId) {
        if let Some(order) = self.pending_orders.get(order_id) {
            if order.owner != from {
                println!("[Exchange {}] CANCEL REJECTED: not owner", self.name);
                return;
            }
        }

        if let Some(_removed) = self.pending_orders.remove(order_id) {
            println!("[Exchange {}] CANCELLED #{} from={}", self.name, order_id, from);
        }
    }

    fn execute_triggered_order(&mut self, sim: &mut dyn SimulatorApi, order: &PendingOrder, now_ns: u64) {
        match order.payload.order_type {
            SimOrderType::Increase => {
                let market_order = MarketOrderPayload {
                    symbol: order.payload.symbol.clone(),
                    side: order.payload.side,
                    qty: order.payload.qty.unwrap_or(0.0),
                    leverage: order.payload.leverage.unwrap_or(5),
                };
                self.process_market_order(sim, order.owner, &market_order, now_ns);
            }
            SimOrderType::Decrease => {
                let close_order = CloseOrderPayload {
                    symbol: order.payload.symbol.clone(),
                    side: order.payload.side,
                };
                self.process_close_order(sim, order.owner, &close_order, now_ns);
            }
        }
    }

    fn cleanup_expired_orders(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        let expired = self.pending_orders.remove_expired(now_ns);
        for order in expired {
            println!("[Exchange {}] EXPIRED #{}", self.name, order.id);
            sim.send(
                self.id,
                order.owner,
                MessageType::OrderCancelled,
                MessagePayload::Text(format!("order_id:{},reason:expired", order.id)),
            );
        }
    }

    fn handle_get_pending_orders(&self, sim: &mut dyn SimulatorApi, keeper_id: AgentId) {
        let mut orders_info = Vec::new();

        for market_cfg in &self.markets {
            let symbol = &market_cfg.symbol;
            for order in self.pending_orders.get_by_symbol(symbol) {
                if let Some(trigger_price) = order.payload.trigger_price {
                    orders_info.push(PendingOrderInfo {
                        order_id: order.id,
                        symbol: order.payload.symbol.clone(),
                        side: order.payload.side,
                        order_type: order.payload.order_type,
                        execution_type: order.payload.execution_type,
                        trigger_price,
                        owner: order.owner,
                    });
                }
            }
        }

        sim.send(
            self.id,
            keeper_id,
            MessageType::PendingOrdersList,
            MessagePayload::PendingOrdersList(PendingOrdersListPayload { orders: orders_info }),
        );
    }

    fn handle_execute_order_from_keeper(
        &mut self,
        sim: &mut dyn SimulatorApi,
        keeper_id: AgentId,
        order_id: OrderId,
        now_ns: u64,
    ) {
        // 1. Check order exists
        let order = match self.pending_orders.get(order_id) {
            Some(o) => o.clone(),
            None => {
                sim.send(
                    self.id,
                    keeper_id,
                    MessageType::OrderAlreadyExecuted,
                    MessagePayload::Text(format!("order_id:{}", order_id)),
                );
                return;
            }
        };

        // 2. Verify trigger still valid
        let symbol = &order.payload.symbol;
        let price = match self.last_prices.get(symbol) {
            Some(&mid) => Price { min: mid, max: mid },
            None => {
                println!(
                    "[Exchange {}] ExecuteOrder rejected: no price for {}",
                    self.name, symbol
                );
                return;
            }
        };

        if !trigger_checker::is_triggered(&order, &price) {
            println!(
                "[Exchange {}] ExecuteOrder rejected: trigger not satisfied for #{}",
                self.name, order_id
            );
            return;
        }

        // 3. Remove and execute
        if let Some(removed_order) = self.pending_orders.remove(order_id) {
            println!(
                "[Exchange {}] KEEPER {} EXECUTES #{} {:?} {:?}",
                self.name, keeper_id, order_id, removed_order.payload.execution_type, removed_order.payload.side
            );

            self.execute_triggered_order(sim, &removed_order, now_ns);

            // 4. Send reward (0.1% of size)
            let size_micro =
                removed_order.payload.qty.unwrap_or(0.0) * self.last_prices.get(symbol).copied().unwrap_or(0) as f64;
            let reward = (size_micro as u64 * 10) / 10000; // 0.1% = 10 bps

            sim.send(
                self.id,
                keeper_id,
                MessageType::KeeperReward,
                MessagePayload::KeeperReward(KeeperRewardPayload {
                    order_id,
                    reward_micro_usd: reward,
                }),
            );
        }
    }

    fn process_close_order(
        &mut self,
        sim: &mut dyn SimulatorApi,
        from: AgentId,
        order: &CloseOrderPayload,
        now_ns: u64,
    ) {
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

        // Calculate PnL before closing using engine's liquidation preview
        let pnl_micro = match self.executor.is_liquidatable_by_margin(now, position_key) {
            Ok(preview) => {
                let pnl = pnl_to_micro_usd(preview.pnl_usd);
                println!(
                    "[Exchange {}] CLOSE PnL preview: is_neg={} pnl_micro={}",
                    self.name, preview.pnl_usd.is_negative, pnl
                );
                pnl
            }
            Err(e) => {
                println!("[Exchange {}] CLOSE PnL error: {:?}", self.name, e);
                0
            }
        };

        // Create decrease order for full position size
        // Note: withdraw_collateral_amount = 0 lets the executor calculate the correct payout
        // after accounting for PnL, fees, etc.
        let perp_order = Order {
            account,
            market_id,
            collateral_token: collateral_asset,
            side,
            order_type: OrderType::Decrease,
            collateral_delta_tokens: U256::zero(),
            size_delta_usd: position.size_usd,        // Close full position
            withdraw_collateral_amount: U256::zero(), // Executor will calculate payout
            target_leverage_x: 0,
            created_at: now,
            valid_from: now,
            valid_until: now + 3600,
        };

        let order_id = self.executor.submit_order(perp_order);

        // Convert to micro-USD for display
        let size_usd_micro = usd_to_micro(position.size_usd);
        let collateral_micro = position.collateral_amount.low_u64();

        match self.executor.execute_order(now, order_id) {
            Ok(()) => {
                println!(
                    "[Exchange {}] CLOSED {} from={} side={:?} size=${:.2}",
                    self.name,
                    order.symbol,
                    from,
                    order.side,
                    size_usd_micro as f64 / 1_000_000.0
                );

                println!(
                    "[Exchange {}] CLOSE PnL: ${:.2} (collateral returned: ${:.2})",
                    self.name,
                    pnl_micro as f64 / 1_000_000.0,
                    collateral_micro as f64 / 1_000_000.0
                );

                // Emit execution event
                sim.emit_event(SimEvent::OrderExecuted {
                    ts: now_ns,
                    account: from,
                    symbol: order.symbol.clone(),
                    side: order.side,
                    size_usd: size_usd_micro,
                    collateral: collateral_micro,
                    execution_price,
                    leverage: 0, // N/A for close
                    order_type: "Decrease".to_string(),
                    pnl: pnl_micro,
                });

                println!(
                    "[Exchange {}] Emitted OrderExecuted from={} pnl={}",
                    self.name, from, pnl_micro
                );

                // Send message to agent with execution details
                // On close: collateral is returned (negative delta), include PnL
                sim.send(
                    self.id,
                    from,
                    MessageType::OrderExecuted,
                    MessagePayload::OrderExecuted(OrderExecutedPayload {
                        symbol: order.symbol.clone(),
                        side: order.side,
                        order_type: OrderExecutionType::Decrease,
                        collateral_delta: -(collateral_micro as i128), // Returned
                        pnl: pnl_micro as i128,
                        size_usd: size_usd_micro as i128,
                    }),
                );

                println!(
                    "[Exchange {}] CLOSE PnL: ${:.2} (collateral returned: ${:.2})",
                    self.name,
                    pnl_micro as f64 / 1_000_000.0,
                    collateral_micro as f64 / 1_000_000.0
                );

                if let Some(market) = self.executor.state.markets.get(&market_id) {
                    println!(
                        "[Exchange {}] {} OI: long=${:.2} short=${:.2}",
                        self.name,
                        order.symbol,
                        usd_to_micro(market.oi_long_usd) as f64 / 1_000_000.0,
                        usd_to_micro(market.oi_short_usd) as f64 / 1_000_000.0
                    );
                }

                // Emit fresh market snapshot for UI updates
                self.emit_market_snapshots_only(sim, now_ns);
                // Broadcast updated market state after close
                self.broadcast_market_state(sim);
            }
            Err(e) => {
                println!(
                    "[Exchange {}] CLOSE REJECTED {} from={} error={}",
                    self.name, order.symbol, from, e
                );
            }
        }
    }

    fn process_market_order(
        &mut self,
        sim: &mut dyn SimulatorApi,
        from: AgentId,
        order: &MarketOrderPayload,
        now_ns: u64,
    ) {
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

        // Get decimals for proper conversion
        let (_index_decimals, collateral_decimals) =
            self.symbol_decimals.get(&order.symbol).copied().unwrap_or((18, 6));

        let account = self.get_or_create_account(from);
        let side = Self::convert_side(order.side);

        // Verify we have oracle prices
        if self.price_cache.borrow().get(&order.symbol).is_none() {
            println!(
                "[Exchange {}] REJECTED from {}: no price for {}",
                self.name, from, order.symbol
            );
            return;
        }

        let now: Timestamp = now_ns / 1_000_000_000;

        // order.qty = number of tokens as f64 (e.g., 0.5 = 0.5 ETH, 2.0 = 2 ETH)
        // Get current price in micro-USD
        let current_price_micro = self.last_prices.get(&order.symbol).copied().unwrap_or(0);

        // size = qty * price (in micro-USD)
        // e.g., 0.5 ETH * $3115 = $1557.50 = 1557_500_000 micro-USD
        let size_micro = (order.qty * current_price_micro as f64) as u64;

        // collateral = size / leverage (in micro-USD)
        // e.g., $1557.50 / 5 = $311.50 = 311_500_000 micro-USD
        let leverage = order.leverage.max(1) as u64;
        let collateral_micro = size_micro / leverage;

        // Convert micro-USD to collateral atoms
        // For USDC (6 decimals): atoms = micro-USD (same scale)
        let collateral_atoms = if collateral_decimals >= 6 {
            U256::from(collateral_micro) * U256::exp10((collateral_decimals - 6) as usize)
        } else {
            U256::from(collateral_micro) / U256::exp10((6 - collateral_decimals) as usize)
        };

        // Convert size to USD(1e30) for engine
        let size_usd_1e30 = U256::from(size_micro) * U256::exp10(24);

        let perp_order = Order {
            account,
            market_id,
            collateral_token: collateral_asset,
            side,
            order_type: OrderType::Increase,
            collateral_delta_tokens: collateral_atoms,
            size_delta_usd: size_usd_1e30, // Explicit size in USD(1e30)
            withdraw_collateral_amount: U256::zero(),
            target_leverage_x: order.leverage.max(1),
            created_at: now,
            valid_from: now,
            valid_until: now + 3600,
        };

        let order_id = self.executor.submit_order(perp_order);

        match self.executor.execute_order(now, order_id) {
            Ok(()) => {
                // Get actual position to report correct values
                let position_key = PositionKey {
                    account,
                    market_id,
                    collateral_token: collateral_asset,
                    side,
                };

                let (size_usd_micro, coll_micro_actual) =
                    if let Some(pos) = self.executor.state.positions.get(&position_key) {
                        let size_m = usd_to_micro(pos.size_usd);
                        // collateral_amount is in atoms (USDC 6 decimals = already micro scale)
                        let coll_m = pos.collateral_amount.low_u64();
                        (size_m, coll_m)
                    } else {
                        // Fallback (shouldn't happen): estimate from order
                        (size_micro, collateral_micro)
                    };

                println!(
                    "[Exchange {}] EXECUTED {} from={} side={:?} collateral=${:.2} size=${:.2} leverage={}x",
                    self.name,
                    order.symbol,
                    from,
                    order.side,
                    coll_micro_actual as f64 / 1_000_000.0,
                    size_usd_micro as f64 / 1_000_000.0,
                    order.leverage
                );

                // Emit execution event
                sim.emit_event(SimEvent::OrderExecuted {
                    ts: now_ns,
                    account: from,
                    symbol: order.symbol.clone(),
                    side: order.side,
                    size_usd: size_usd_micro,
                    collateral: coll_micro_actual,
                    execution_price: current_price_micro,
                    leverage: order.leverage,
                    order_type: "Increase".to_string(),
                    pnl: 0, // No PnL on open
                });

                // Send message to agent with execution details
                // On open: collateral is locked (positive delta)
                sim.send(
                    self.id,
                    from,
                    MessageType::OrderExecuted,
                    MessagePayload::OrderExecuted(OrderExecutedPayload {
                        symbol: order.symbol.clone(),
                        side: order.side,
                        order_type: OrderExecutionType::Increase,
                        collateral_delta: coll_micro_actual as i128, // Locked
                        pnl: 0,
                        size_usd: size_usd_micro as i128,
                    }),
                );

                if let Some(market) = self.executor.state.markets.get(&market_id) {
                    println!(
                        "[Exchange {}] {} OI: long=${:.2} short=${:.2}",
                        self.name,
                        order.symbol,
                        usd_to_micro(market.oi_long_usd) as f64 / 1_000_000.0,
                        usd_to_micro(market.oi_short_usd) as f64 / 1_000_000.0
                    );
                }

                // Emit fresh market snapshot for UI updates
                self.emit_market_snapshots_only(sim, now_ns);
                // Broadcast updated market state after open
                self.broadcast_market_state(sim);
            }
            Err(e) => {
                println!(
                    "[Exchange {}] REJECTED {} from={} error={}",
                    self.name, order.symbol, from, e
                );
            }
        }
    }

    fn handle_preview_request(
        &mut self,
        sim: &mut dyn SimulatorApi,
        from: AgentId,
        req: PreviewRequestPayload,
        now_ns: u64,
    ) {
        let mut response = PreviewResponsePayload {
            success: false,
            message: "preview_failed".to_string(),
            symbol: req.symbol.clone(),
            side: req.side,
            qty: req.qty,
            leverage: req.leverage,
            size_usd: 0,
            collateral: 0,
            entry_price: 0,
            current_price: 0,
            liquidation_price: 0,
        };

        let (market_id, collateral_asset) = match self.symbol_to_market.get(&req.symbol) {
            Some(m) => *m,
            None => {
                response.message = format!("unknown symbol: {}", req.symbol);
                sim.send(
                    self.id,
                    from,
                    MessageType::PreviewResponse,
                    MessagePayload::PreviewResponse(response),
                );
                return;
            }
        };

        if self.price_cache.borrow().get(&req.symbol).is_none() {
            response.message = format!("no price for {}", req.symbol);
            sim.send(
                self.id,
                from,
                MessageType::PreviewResponse,
                MessagePayload::PreviewResponse(response),
            );
            return;
        }

        let (index_decimals, collateral_decimals) = self.symbol_decimals.get(&req.symbol).copied().unwrap_or((18, 6));

        let current_price_micro = self.last_prices.get(&req.symbol).copied().unwrap_or(0);
        if current_price_micro == 0 {
            response.message = "current price unavailable".to_string();
            sim.send(
                self.id,
                from,
                MessageType::PreviewResponse,
                MessagePayload::PreviewResponse(response),
            );
            return;
        }

        if req.qty <= 0.0 {
            response.message = "qty must be > 0".to_string();
            sim.send(
                self.id,
                from,
                MessageType::PreviewResponse,
                MessagePayload::PreviewResponse(response),
            );
            return;
        }

        let leverage = req.leverage.max(1) as u64;
        let size_micro = (req.qty * current_price_micro as f64) as u64;
        let collateral_micro = size_micro / leverage;

        let collateral_atoms = if collateral_decimals >= 6 {
            U256::from(collateral_micro) * U256::exp10((collateral_decimals - 6) as usize)
        } else {
            U256::from(collateral_micro) / U256::exp10((6 - collateral_decimals) as usize)
        };

        let size_usd_1e30 = U256::from(size_micro) * U256::exp10(24);
        let side = Self::convert_side(req.side);
        let account = self.get_or_create_account(from);
        let now_sec: Timestamp = now_ns / 1_000_000_000;

        let oracle = SimOracle::new(self.price_cache.clone(), &self.markets);
        let mut preview_executor = Executor::new(self.executor.state.clone(), BasicServicesBundle::default(), oracle);

        let perp_order = Order {
            account,
            market_id,
            collateral_token: collateral_asset,
            side,
            order_type: OrderType::Increase,
            collateral_delta_tokens: collateral_atoms,
            size_delta_usd: size_usd_1e30,
            withdraw_collateral_amount: U256::zero(),
            target_leverage_x: req.leverage.max(1),
            created_at: now_sec,
            valid_from: now_sec,
            valid_until: now_sec + 3600,
        };

        let order_id = preview_executor.submit_order(perp_order);
        if let Err(e) = preview_executor.execute_order(now_sec, order_id) {
            response.message = format!("preview execute failed: {:?}", e);
            sim.send(
                self.id,
                from,
                MessageType::PreviewResponse,
                MessagePayload::PreviewResponse(response),
            );
            return;
        }

        let position_key = PositionKey {
            account,
            market_id,
            collateral_token: collateral_asset,
            side,
        };

        let position = match preview_executor.state.positions.get(&position_key) {
            Some(pos) => pos,
            None => {
                response.message = "preview position not found".to_string();
                sim.send(
                    self.id,
                    from,
                    MessageType::PreviewResponse,
                    MessagePayload::PreviewResponse(response),
                );
                return;
            }
        };

        let entry_price_atom = if !position.size_tokens.is_zero() {
            position.size_usd / position.size_tokens
        } else {
            U256::zero()
        };
        let entry_price_micro = denormalize_price_from_atom(entry_price_atom, index_decimals);

        let liq_price_micro = match preview_executor.calculate_liquidation_price(now_sec, position_key) {
            Ok(liq_price_atom) => denormalize_price_from_atom(liq_price_atom, index_decimals),
            Err(e) => {
                response.message = format!("liquidation price calc failed: {:?}", e);
                sim.send(
                    self.id,
                    from,
                    MessageType::PreviewResponse,
                    MessagePayload::PreviewResponse(response),
                );
                return;
            }
        };

        let size_usd_micro = usd_to_micro(position.size_usd) as i128;
        let collateral_micro_actual = position.collateral_amount.low_u64() as i128;

        response.success = true;
        response.message = "ok".to_string();
        response.size_usd = size_usd_micro;
        response.collateral = collateral_micro_actual;
        response.entry_price = entry_price_micro;
        response.current_price = current_price_micro;
        response.liquidation_price = liq_price_micro;

        sim.send(
            self.id,
            from,
            MessageType::PreviewResponse,
            MessagePayload::PreviewResponse(response),
        );
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
                // Convert from USD(1e30) to human-readable USD
                let oi_long = usd_to_micro(market.oi_long_usd) as f64 / 1_000_000.0;
                let oi_short = usd_to_micro(market.oi_short_usd) as f64 / 1_000_000.0;
                println!(
                    "[Exchange {}] {} OI: long=${:.2} short=${:.2}",
                    self.name, market_cfg.symbol, oi_long, oi_short
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
                    if self.symbol_to_market.contains_key(symbol) {
                        self.price_cache.borrow_mut().update(symbol, price.min, price.max);
                        let mid_price = (price.min + price.max) / 2;
                        self.last_prices.insert(symbol.clone(), mid_price);

                        let now_ns = sim.now_ns();
                        let _liquidatable = self.emit_snapshots(sim, now_ns);
                        self.broadcast_market_state(sim);

                        // Keeper'ы сами проверяют триггеры, тут только cleanup
                        self.cleanup_expired_orders(sim, now_ns);
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

            MessageType::SubmitOrder => {
                if let MessagePayload::Order(order) = &msg.payload {
                    let now_ns = sim.now_ns();
                    self.process_submit_order(sim, msg.from, order, now_ns);
                }
            }

            MessageType::CancelOrder => {
                if let MessagePayload::CancelOrder(payload) = &msg.payload {
                    self.process_cancel_order(sim, msg.from, payload.order_id);
                }
            }

            MessageType::LimitOrder => {
                println!(
                    "[Exchange {}] LIMIT_ORDER from {} (use SubmitOrder instead)",
                    self.name, msg.from
                );
            }

            MessageType::LiquidationScan => {
                let now_ns = sim.now_ns();
                self.process_liquidation_scan(sim, now_ns);
            }
            MessageType::PreviewRequest => {
                if let MessagePayload::PreviewRequest(payload) = &msg.payload {
                    let now_ns = sim.now_ns();
                    self.handle_preview_request(sim, msg.from, payload.clone(), now_ns);
                }
            }

            MessageType::GetPendingOrders => {
                self.handle_get_pending_orders(sim, msg.from);
            }

            MessageType::ExecuteOrder => {
                if let MessagePayload::ExecuteOrder(payload) = &msg.payload {
                    let now_ns = sim.now_ns();
                    self.handle_execute_order_from_keeper(sim, msg.from, payload.order_id, now_ns);
                }
            }

            _ => {}
        }
    }
}
