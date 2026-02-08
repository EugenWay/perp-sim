use crate::agents::Agent;
use crate::messages::{
    AgentId, CloseOrderPayload, ExecutionType, KeeperRewardPayload, MarketOrderPayload, MarketStatePayload, Message,
    MessagePayload, MessageType, OracleTickPayload, OrderId, OrderPayload,
    OrderType as SimOrderType, PendingOrderInfo, PendingOrdersListPayload,
    PreviewRequestPayload, PreviewResponsePayload, Price, Side as SimSide, SimulatorApi,
};
use crate::pending_orders::{PendingOrder, PendingOrderStore};
use crate::trigger_checker;
use crate::vara::{
    ActorId, ExecutionType as VaraExecutionType, OracleInput, OraclePrices, Order as VaraOrder,
    OrderId as VaraOrderId, OrderType as VaraOrderType, PositionKey as VaraPositionKey,
    Side as VaraSide, TxResult, VaraClient, u256_from_sails, u256_to_sails,
};
use std::collections::{HashMap, HashSet};
use primitive_types::U256;
use std::io::{BufWriter, Write};
use std::sync::Arc;

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

#[derive(Clone)]
struct PriceCache {
    /// Maps symbol -> (index_price_min, index_price_max) in USD(1e30) per atom
    prices: HashMap<String, (U256, U256)>,
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

    fn get(&self, symbol: &str) -> Option<(U256, U256)> {
        self.prices.get(symbol).copied()
    }
}

// ==== ExchangeAgent (on-chain only) ====

pub struct ExchangeAgent {
    id: AgentId,
    name: String,
    markets: Vec<MarketConfig>,
    last_prices: HashMap<String, u64>,

    price_cache: PriceCache,
    actor_ids: HashMap<AgentId, ActorId>,
    symbols: HashSet<String>,
    symbol_decimals: HashMap<String, (u32, u32)>,

    pending_orders: PendingOrderStore,
    vara_client: Arc<VaraClient>,
    poll_interval_ns: u64,

    /// Receiver for on-chain transaction results from VaraClient
    tx_result_rx: Option<crossbeam_channel::Receiver<TxResult>>,
    /// CSV writer for transaction results log
    tx_csv_writer: Option<BufWriter<std::fs::File>>,

    /// Channel for async OI sync results from background RPC
    oi_sync_tx: crossbeam_channel::Sender<(i128, i128)>,
    oi_sync_rx: crossbeam_channel::Receiver<(i128, i128)>,
    /// Whether an OI fetch is currently in-flight
    oi_sync_pending: bool,
}

impl ExchangeAgent {
    pub fn new(
        id: AgentId,
        name: String,
        markets: Vec<MarketConfig>,
        vara_client: Arc<VaraClient>,
        tx_result_rx: Option<crossbeam_channel::Receiver<TxResult>>,
        logs_dir: Option<&str>,
    ) -> Self {
        let mut price_cache = PriceCache::new();
        let mut symbols = HashSet::new();
        let mut symbol_decimals = HashMap::new();

        for market_cfg in markets.iter() {
            price_cache.set_decimals(&market_cfg.symbol, market_cfg.index_decimals);

            symbols.insert(market_cfg.symbol.clone());
            symbol_decimals.insert(
                market_cfg.symbol.clone(),
                (market_cfg.index_decimals, market_cfg.collateral_decimals),
            );

            println!(
                "[Exchange {}] Market {} ({}) initialized: liquidity=${:.0}M",
                name,
                market_cfg.symbol,
                market_cfg.id,
                market_cfg.liquidity_usd as f64 / 1_000_000_000_000.0,
            );
        }

        // Create CSV writer for transaction results
        let tx_csv_writer = logs_dir.and_then(|dir| {
            let path = format!("{}/transactions.csv", dir);
            match std::fs::File::create(&path) {
                Ok(file) => {
                    let mut writer = BufWriter::new(file);
                    let _ = writeln!(writer, "agent_id,tx_type,success,order_id,error,detail");
                    println!("[Exchange {}] Transaction log: {}", name, path);
                    Some(writer)
                }
                Err(e) => {
                    eprintln!("[Exchange {}] Failed to create {}: {}", name, path, e);
                    None
                }
            }
        });

        let (oi_sync_tx, oi_sync_rx) = crossbeam_channel::unbounded();

        Self {
            id,
            name,
            markets,
            last_prices: HashMap::new(),
            price_cache,
            actor_ids: HashMap::new(),
            symbols,
            symbol_decimals,
            pending_orders: PendingOrderStore::new(),
            vara_client,
            poll_interval_ns: 3_000_000_000,
            tx_result_rx,
            tx_csv_writer,
            oi_sync_tx,
            oi_sync_rx,
            oi_sync_pending: false,
        }
    }

    fn get_or_create_actor(&mut self, agent_id: AgentId) -> Option<ActorId> {
        if let Some(actor) = self.actor_ids.get(&agent_id) {
            return Some(*actor);
        }
        match self.vara_client.get_actor_id(agent_id) {
            Ok(actor) => {
                self.actor_ids.insert(agent_id, actor);
                Some(actor)
            }
            Err(e) => {
                eprintln!("[Exchange {}] Failed to get ActorId for {}: {}", self.name, agent_id, e);
                None
            }
        }
    }

    fn convert_side_to_vara(side: SimSide) -> VaraSide {
        match side {
            SimSide::Buy => VaraSide::Long,
            SimSide::Sell => VaraSide::Short,
        }
    }

    fn build_oracle_input(&self, symbol: &str) -> Option<OracleInput> {
        let (min, max) = self.price_cache.get(symbol)?;
        let collateral_price = U256::exp10(24); // USDC $1 with 6 decimals
        let prices = OraclePrices {
            index_price_min: u256_to_sails(min),
            index_price_max: u256_to_sails(max),
            collateral_price_min: u256_to_sails(collateral_price),
            collateral_price_max: u256_to_sails(collateral_price),
        };
        Some(OracleInput::DevPrices(prices))
    }

    /// Drain all pending transaction results from the channel.
    /// Logs each to CSV and sends failure notifications back to agents.
    fn drain_tx_results(&mut self, sim: &mut dyn SimulatorApi) {
        let rx = match &self.tx_result_rx {
            Some(rx) => rx,
            None => return,
        };

        while let Ok(result) = rx.try_recv() {
            // Log to CSV
            if let Some(writer) = &mut self.tx_csv_writer {
                let oid = result.order_id.map(|id| id.to_string()).unwrap_or_default();
                let err = result.error.as_deref().unwrap_or("");
                let _ = writeln!(
                    writer,
                    "{},{},{},{},{},\"{}\"",
                    result.agent_id, result.tx_type, result.success, oid, err, result.detail
                );
                let _ = writer.flush();
            }

            // Notify agent on failure
            if !result.success {
                let reason = result.error.as_deref().unwrap_or("unknown");
                println!(
                    "[Exchange {}] TX FAILED: agent={} {} — {}",
                    self.name, result.agent_id, result.tx_type, reason
                );
                sim.send(
                    self.id,
                    result.agent_id,
                    MessageType::OrderRejected,
                    MessagePayload::Text(format!(
                        "tx_type:{},order_id:{},error:{}",
                        result.tx_type,
                        result.order_id.unwrap_or(0),
                        reason
                    )),
                );
            }
        }
    }

    /// Start an asynchronous OI fetch if none is already in-flight.
    /// The RPC call runs on VaraClient's blocking thread pool; result arrives via oi_sync_rx.
    fn start_oi_fetch(&mut self) {
        if self.oi_sync_pending {
            return; // previous fetch still running
        }
        self.vara_client.fetch_oi_async(self.oi_sync_tx.clone());
        self.oi_sync_pending = true;
    }

    /// Drain completed OI sync results and broadcast MarketState to all agents.
    /// Non-blocking: if no result is ready yet, this is a no-op.
    fn drain_oi_sync(&mut self, sim: &mut dyn SimulatorApi) {
        let (oi_long_usd, oi_short_usd) = match self.oi_sync_rx.try_recv() {
            Ok(oi) => {
                self.oi_sync_pending = false;
                oi
            }
            Err(crossbeam_channel::TryRecvError::Empty) => return,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                eprintln!("[Exchange {}] OI sync channel disconnected", self.name);
                return;
            }
        };

        let symbol = match self.markets.first() {
            Some(m) => m.symbol.clone(),
            None => return,
        };

        let liquidity_usd = self.markets.first().map(|m| m.liquidity_usd).unwrap_or_default();

        let payload = MarketStatePayload {
            symbol,
            oi_long_usd,
            oi_short_usd,
            liquidity_usd,
        };

        sim.broadcast(self.id, MessageType::MarketState, MessagePayload::MarketState(payload));
    }

    /// On-chain liquidations are handled by keepers/contract — no-op here.
    fn process_liquidation_scan(&mut self, _sim: &mut dyn SimulatorApi, _now_ns: u64) {}

    fn validate_order(&self, order: &OrderPayload) -> Result<(), String> {
        if !self.symbols.contains(&order.symbol) {
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
        let actor = match self.get_or_create_actor(from) {
            Some(a) => a,
            None => return,
        };
        let now_sec: u64 = now_ns / 1_000_000_000;

        let (index_decimals, collateral_decimals) =
            self.symbol_decimals.get(&order.symbol).copied().unwrap_or((18, 6));

        let current_price_micro = self.last_prices.get(&order.symbol).copied().unwrap_or(0);

        let (collateral_atoms, size_usd_1e30, target_leverage_x) = match order.order_type {
            SimOrderType::Increase => {
                let qty = order.qty.unwrap_or(0.0);
                let leverage = order.leverage.unwrap_or(5).max(1);
                let size_micro = (qty * current_price_micro as f64) as u64;
                let collateral_micro = size_micro / leverage as u64;

                let collateral_atoms = if collateral_decimals >= 6 {
                    U256::from(collateral_micro) * U256::exp10((collateral_decimals - 6) as usize)
                } else {
                    U256::from(collateral_micro) / U256::exp10((6 - collateral_decimals) as usize)
                };
                let size_usd_1e30 = U256::from(size_micro) * U256::exp10(24);
                (u256_to_sails(collateral_atoms), u256_to_sails(size_usd_1e30), leverage)
            }
            SimOrderType::Decrease => {
                let size_micro = if let Some(size) = order.size_delta_usd {
                    size
                } else {
                    let side = Self::convert_side_to_vara(order.side);
                    let position_key = VaraPositionKey { account: actor, side };
                    match self.vara_client.get_position(&position_key) {
                        Ok(Some(p)) => usd_to_micro(u256_from_sails(p.size_usd)),
                        _ => 0,
                    }
                };
                let size_usd_1e30 = U256::from(size_micro) * U256::exp10(24);
                (u256_to_sails(U256::zero()), u256_to_sails(size_usd_1e30), 0)
            }
        };

        let trigger_price = order
            .trigger_price
            .map(|p| u256_to_sails(normalize_price_to_atom(p, index_decimals)));
        let acceptable_price = order
            .acceptable_price
            .map(|p| u256_to_sails(normalize_price_to_atom(p, index_decimals)));

        let execution_type = match order.execution_type {
            ExecutionType::Market => VaraExecutionType::Market,
            ExecutionType::Limit => VaraExecutionType::Limit,
            ExecutionType::StopLoss => VaraExecutionType::StopLoss,
            ExecutionType::TakeProfit => VaraExecutionType::TakeProfit,
        };

        let order_type = match order.order_type {
            SimOrderType::Increase => VaraOrderType::Increase,
            SimOrderType::Decrease => VaraOrderType::Decrease,
        };

        let valid_for = order.valid_for_sec.unwrap_or(3600);

        let onchain_order = VaraOrder {
            account: actor,
            side: Self::convert_side_to_vara(order.side),
            order_type,
            execution_type,
            collateral_delta_tokens: collateral_atoms,
            size_delta_usd: size_usd_1e30,
            trigger_price,
            acceptable_price,
            withdraw_collateral_amount: u256_to_sails(U256::zero()),
            target_leverage_x,
            created_at: now_sec,
            valid_from: now_sec,
            valid_until: now_sec + valid_for,
        };

        // Fire-and-forget: submit limit/stop/TP order to chain
        if let Err(e) = self.vara_client.submit_order(from, &onchain_order) {
            eprintln!(
                "[Exchange {}] SubmitOrder failed from {}: {}",
                self.name, from, e
            );
            return;
        }

        println!(
            "[Exchange {}] SUBMITTED LIMIT from={} {:?} {:?} trigger=${:.2}",
            self.name,
            from,
            order.execution_type,
            order.side,
            order.trigger_price.unwrap_or(0) as f64 / 1_000_000.0
        );

        sim.send(
            self.id,
            from,
            MessageType::OrderPending,
            MessagePayload::Text("order submitted on-chain".to_string()),
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

    fn execute_triggered_order(&mut self, keeper_id: AgentId, order: &PendingOrder) {
        let oracle_input = match self.build_oracle_input(&order.payload.symbol) {
            Some(input) => input,
            None => {
                eprintln!(
                    "[Exchange {}] ExecuteOrder: no oracle price for {}",
                    self.name, order.payload.symbol
                );
                return;
            }
        };

        let order_id = VaraOrderId(order.id);
        if let Err(e) = self.vara_client.execute_order(keeper_id, order_id, &oracle_input) {
            eprintln!(
                "[Exchange {}] ExecuteOrder failed id={} by keeper {}: {}",
                self.name, order.id, keeper_id, e
            );
        } else {
            println!(
                "[Exchange {}] ON-CHAIN EXECUTE #{} by keeper {}",
                self.name, order.id, keeper_id
            );
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
        _now_ns: u64,
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

            self.execute_triggered_order(keeper_id, &removed_order);

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
        _sim: &mut dyn SimulatorApi,
        from: AgentId,
        order: &CloseOrderPayload,
        now_ns: u64,
    ) {
        if !self.symbols.contains(&order.symbol) {
            println!(
                "[Exchange {}] CLOSE REJECTED from {}: unknown symbol {}",
                self.name, from, order.symbol
            );
            return;
        }

        let actor = match self.get_or_create_actor(from) {
            Some(a) => a,
            None => return,
        };
        let side = Self::convert_side_to_vara(order.side);
        let now_sec: u64 = now_ns / 1_000_000_000;

        let position_key = VaraPositionKey { account: actor, side: side.clone() };
        let position = match self.vara_client.get_position(&position_key) {
            Ok(Some(p)) if !p.size_usd.is_zero() => p,
            Ok(_) => {
                println!(
                    "[Exchange {}] CLOSE REJECTED from {}: no {:?} position for {}",
                    self.name, from, order.side, order.symbol
                );
                return;
            }
            Err(e) => {
                eprintln!("[Exchange {}] CLOSE query failed: {}", self.name, e);
                return;
            }
        };

        let onchain_order = VaraOrder {
            account: actor,
            side,
            order_type: VaraOrderType::Decrease,
            execution_type: VaraExecutionType::Market,
            collateral_delta_tokens: u256_to_sails(U256::zero()),
            size_delta_usd: position.size_usd,
            trigger_price: None,
            acceptable_price: None,
            withdraw_collateral_amount: u256_to_sails(U256::zero()),
            target_leverage_x: 0,
            created_at: now_sec,
            valid_from: now_sec,
            valid_until: now_sec + 3600,
        };

        // Build oracle input before spawning
        let oracle_input = match self.build_oracle_input(&order.symbol) {
            Some(oi) => oi,
            None => {
                eprintln!(
                    "[Exchange {}] no oracle input for {}, skipping close order",
                    self.name, order.symbol
                );
                return;
            }
        };

        println!(
            "[Exchange {}] ON-CHAIN CLOSE {} from={} side={:?}",
            self.name, order.symbol, from, order.side
        );

        // Fire-and-forget: submit + execute runs in background
        if let Err(e) = self.vara_client.submit_and_execute_order_async(from, onchain_order, oracle_input) {
            eprintln!(
                "[Exchange {}] submit_and_execute_order_async(close) failed {} from={}: {}",
                self.name, order.symbol, from, e
            );
        }
    }

    fn process_market_order(
        &mut self,
        _sim: &mut dyn SimulatorApi,
        from: AgentId,
        order: &MarketOrderPayload,
        now_ns: u64,
    ) {
        if !self.symbols.contains(&order.symbol) {
            println!(
                "[Exchange {}] REJECTED from {}: unknown symbol {}",
                self.name, from, order.symbol
            );
            return;
        }

        // Get decimals for proper conversion
        let (_index_decimals, collateral_decimals) =
            self.symbol_decimals.get(&order.symbol).copied().unwrap_or((18, 6));

        // Verify we have oracle prices
        if self.price_cache.get(&order.symbol).is_none() {
            println!(
                "[Exchange {}] REJECTED from {}: no price for {}",
                self.name, from, order.symbol
            );
            return;
        }

        let now_sec: u64 = now_ns / 1_000_000_000;
        let actor = match self.get_or_create_actor(from) {
            Some(a) => a,
            None => return,
        };
        let side = Self::convert_side_to_vara(order.side);

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

        let onchain_order = VaraOrder {
            account: actor,
            side,
            order_type: VaraOrderType::Increase,
            execution_type: VaraExecutionType::Market,
            collateral_delta_tokens: u256_to_sails(collateral_atoms),
            size_delta_usd: u256_to_sails(size_usd_1e30),
            trigger_price: None,
            acceptable_price: None,
            withdraw_collateral_amount: u256_to_sails(U256::zero()),
            target_leverage_x: order.leverage.max(1),
            created_at: now_sec,
            valid_from: now_sec,
            valid_until: now_sec + 3600,
        };

        // Build oracle input before spawning
        let oracle_input = match self.build_oracle_input(&order.symbol) {
            Some(oi) => oi,
            None => {
                eprintln!(
                    "[Exchange {}] no oracle input for {}, skipping market order",
                    self.name, order.symbol
                );
                return;
            }
        };

        println!(
            "[Exchange {}] ON-CHAIN MARKET {} from={} side={:?} size=${:.2} leverage={}x",
            self.name,
            order.symbol,
            from,
            order.side,
            size_micro as f64 / 1_000_000.0,
            order.leverage
        );

        // Fire-and-forget: submit + execute runs in background, does NOT block the kernel
        if let Err(e) = self.vara_client.submit_and_execute_order_async(from, onchain_order, oracle_input) {
            eprintln!(
                "[Exchange {}] submit_and_execute_order_async failed {} from={}: {}",
                self.name, order.symbol, from, e
            );
        }
    }

    fn handle_preview_request(
        &mut self,
        sim: &mut dyn SimulatorApi,
        from: AgentId,
        req: PreviewRequestPayload,
        now_ns: u64,
    ) {
        let _ = now_ns;
        let response = PreviewResponsePayload {
            success: false,
            message: "preview not supported (on-chain only)".to_string(),
            symbol: req.symbol.clone(),
            side: req.side,
            qty: req.qty,
            leverage: req.leverage,
            size_usd: 0,
            collateral: 0,
            entry_price: 0,
            current_price: 0,
            liquidation_price: 0,
            funding_fee_usd: 0,
            borrowing_fee_usd: 0,
            price_impact_usd: 0,
            close_fees_usd: 0,
        };

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

    fn on_start(&mut self, sim: &mut dyn SimulatorApi) {
        println!("[Exchange {}] started with {} market(s)", self.name, self.markets.len());
        sim.wakeup(self.id, sim.now_ns() + self.poll_interval_ns);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, now_ns: u64) {
        self.drain_tx_results(sim);
        self.drain_oi_sync(sim);    // non-blocking: process result if ready
        self.start_oi_fetch();       // kick off next async RPC fetch
        sim.wakeup(self.id, now_ns + self.poll_interval_ns);
    }

    fn on_stop(&mut self, _sim: &mut dyn SimulatorApi) {
        println!("[Exchange {}] stopped", self.name);
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
                    if self.symbols.contains(symbol) {
                        self.price_cache.update(symbol, price.min, price.max);
                        let mid_price = (price.min + price.max) / 2;
                        self.last_prices.insert(symbol.clone(), mid_price);

                        let now_ns = sim.now_ns();
                        // sync_from_chain runs on wakeup every poll_interval — no need to duplicate here
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
