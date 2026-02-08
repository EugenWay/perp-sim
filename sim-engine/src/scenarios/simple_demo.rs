use crate::agents::{
    exchange_agent::{ExchangeAgent, MarketConfig},
    human_agent::HumanAgent,
    keeper_agent::{KeeperAgent, KeeperConfig},
    limit_trader_agent::{LimitStrategy, LimitTraderAgent, LimitTraderConfig, OrderMode},
    liquidation_agent::LiquidationAgent,
    market_maker_agent::{MarketMakerAgent, MarketMakerConfig},
    oracle_agent::OracleAgent,
    smart_trader_agent::{SmartTraderAgent, SmartTraderConfig, TradingStrategy},
};
use crate::api::{CachedPriceProvider, PythProvider};
use crate::events::{EventListener, SimEvent};
use crate::logging::{CsvExecutionLogger, CsvLiquidationLogger, CsvMarketLogger, CsvOracleLogger, CsvPositionLogger};
use crate::messages::Side;
use crate::sim_engine::SimEngine;
use crate::vara::VaraClient;
use crate::vara::keystore::normalize_agent_id;
use primitive_types::U256;

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::sync::Arc;

struct ClosureListener<F: FnMut(&SimEvent)> {
    closure: F,
}

impl<F: FnMut(&SimEvent)> EventListener for ClosureListener<F> {
    fn on_event(&mut self, event: &SimEvent) {
        (self.closure)(event);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LiquidityConfig {
    collateral_amount: i128,
    index_amount: i128,
    liquidity_usd: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AddressEntry {
    name: String,
    address: String,
}

struct AddressBook {
    entries: HashMap<String, String>,
}

impl AddressBook {
    fn load() -> Self {
        // Try to find addresses.json in multiple locations
        let path = std::env::var("VARA_ADDRESS_BOOK").ok().or_else(|| {
            let candidates = [
                "keys/addresses.json",
                "../keys/addresses.json",
                "../../keys/addresses.json",
            ];
            candidates
                .iter()
                .find(|p| std::path::Path::new(p).exists())
                .map(|s| s.to_string())
        });

        let entries = match path {
            Some(path) => match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<Vec<AddressEntry>>(&content) {
                    Ok(list) => {
                        println!("[AddressBook] Loaded {} addresses from {}", list.len(), path);
                        list.into_iter().map(|e| (e.name, e.address)).collect()
                    }
                    Err(e) => {
                        eprintln!("[AddressBook] Failed to parse {}: {}", path, e);
                        HashMap::new()
                    }
                },
                Err(e) => {
                    eprintln!("[AddressBook] Failed to read {}: {}", path, e);
                    HashMap::new()
                }
            },
            None => {
                eprintln!("[AddressBook] addresses.json not found (set VARA_ADDRESS_BOOK or place in keys/)");
                HashMap::new()
            }
        };

        Self { entries }
    }

    fn address_for_agent(&self, id: u32) -> Option<String> {
        let normalized = normalize_agent_id(id);
        let name = format!("bot_{:03}", normalized);
        self.entries.get(&name).cloned()
    }

    fn write_funding_list(&self, path: &str) {
        if self.entries.is_empty() {
            return; // Don't write empty file
        }

        // Try to find the correct keys directory
        let actual_path = if std::path::Path::new("keys").exists() {
            path.to_string()
        } else if std::path::Path::new("../keys").exists() {
            path.replace("keys/", "../keys/")
        } else {
            path.to_string()
        };

        let mut items: Vec<_> = self.entries.iter().collect();
        items.sort_by_key(|(name, _)| *name);
        let mut lines = Vec::with_capacity(items.len());
        for (name, address) in items {
            lines.push(format!("{name} {address}"));
        }
        if let Err(e) = std::fs::write(&actual_path, lines.join("\n")) {
            eprintln!("[AddressBook] Failed to write funding list {}: {}", actual_path, e);
        } else {
            println!("[AddressBook] Funding list written to {}", actual_path);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MarketJsonConfig {
    id: u32,
    symbol: String,
    index_token: String,
    #[serde(default = "default_index_decimals")]
    index_decimals: u32,
    collateral_token: String,
    #[serde(default = "default_collateral_decimals")]
    collateral_decimals: u32,
    initial_liquidity: LiquidityConfig,
}

fn default_index_decimals() -> u32 {
    18 // ETH default
}

fn default_collateral_decimals() -> u32 {
    6 // USDT default
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExchangeConfig {
    id: u32,
    name: String,
    markets: Vec<MarketJsonConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OracleConfig {
    id: u32,
    name: String,
    symbols: Vec<String>,
    provider: String,
    cache_duration_ms: u64,
    #[serde(default = "default_wake_interval")]
    wake_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SmartTraderJsonConfig {
    id: u32,
    name: String,
    symbol: String,
    strategy: String,
    #[serde(default = "default_side")]
    side: String,
    #[serde(default = "default_leverage")]
    leverage: u32,
    #[serde(default = "default_qty")]
    qty: u64,
    #[serde(default)]
    qty_min: Option<f64>,
    #[serde(default)]
    qty_max: Option<f64>,
    #[serde(default = "default_hold_duration")]
    hold_duration_sec: u64,
    #[serde(default = "default_lookback")]
    lookback_sec: u64,
    #[serde(default = "default_threshold")]
    threshold_pct: f64,
    #[serde(default = "default_smart_wake_interval")]
    wake_interval_ms: u64,
    #[serde(default)]
    take_profit_pct: Option<f64>,
    #[serde(default)]
    stop_loss_pct: Option<f64>,
    #[serde(default)]
    min_hold_sec: Option<u64>,
    #[serde(default)]
    max_hold_sec: Option<u64>,
    #[serde(default)]
    reentry_delay_sec: Option<u64>,
    #[serde(default)]
    lookback_periods: Option<u32>,
    #[serde(default)]
    entry_deviation_pct: Option<f64>,
    #[serde(default)]
    exit_deviation_pct: Option<f64>,
    #[serde(default)]
    balance: Option<i128>,
    #[serde(default)]
    start_delay_ms: Option<u64>,
}

fn default_side() -> String {
    "long".to_string()
}

fn default_leverage() -> u32 {
    5
}

fn default_qty() -> u64 {
    1
}

fn default_hold_duration() -> u64 {
    60
}

fn default_lookback() -> u64 {
    30
}

fn default_threshold() -> f64 {
    0.5
}

fn default_smart_wake_interval() -> u64 {
    5000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LiquidationAgentConfig {
    id: u32,
    name: String,
    #[serde(default = "default_liquidation_wake_interval")]
    wake_interval_ms: u64,
}

fn default_liquidation_wake_interval() -> u64 {
    200 // 200ms default scan interval
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MarketMakerJsonConfig {
    id: u32,
    name: String,
    symbol: String,
    #[serde(default = "default_mm_target_oi")]
    target_oi_per_side: i128,
    #[serde(default = "default_mm_max_imbalance")]
    max_imbalance_pct: f64,
    #[serde(default = "default_mm_order_size")]
    order_size_tokens: f64,
    #[serde(default = "default_mm_leverage")]
    leverage: u32,
    #[serde(default = "default_mm_wake_interval")]
    wake_interval_ms: u64,
    #[serde(default = "default_mm_balance")]
    balance: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LimitTraderJsonConfig {
    id: u32,
    name: String,
    symbol: String,
    strategy: String,
    #[serde(default = "default_leverage")]
    leverage: u32,
    #[serde(default)]
    qty: f64,
    #[serde(default = "default_limit_wake_interval")]
    wake_interval_ms: u64,
    #[serde(default)]
    balance: Option<i128>,
    #[serde(default)]
    entry_offset_pct: Option<f64>,
    #[serde(default)]
    breakout_offset_pct: Option<f64>,
    #[serde(default)]
    stop_loss_pct: Option<f64>,
    #[serde(default)]
    take_profit_pct: Option<f64>,
    #[serde(default)]
    trend_lookback: Option<u32>,
    #[serde(default)]
    direction: Option<String>,
    // Smart strategy fields
    #[serde(default)]
    sma_fast: Option<u32>,
    #[serde(default)]
    sma_slow: Option<u32>,
    #[serde(default)]
    rsi_period: Option<u32>,
    #[serde(default)]
    rsi_low: Option<f64>,
    #[serde(default)]
    rsi_high: Option<f64>,
    #[serde(default)]
    atr_period: Option<u32>,
    #[serde(default)]
    entry_atr_mult: Option<f64>,
    #[serde(default)]
    stop_atr_mult: Option<f64>,
    #[serde(default)]
    take_atr_mult: Option<f64>,
    #[serde(default)]
    order_mode: Option<String>,
}

fn default_limit_wake_interval() -> u64 {
    2000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeeperJsonConfig {
    id: u32,
    name: String,
    #[serde(default = "default_keeper_wake_interval")]
    wake_interval_ms: u64,
}

fn default_keeper_wake_interval() -> u64 {
    200
}

fn default_mm_target_oi() -> i128 {
    150_000_000_000 // $150k per side
}
fn default_mm_max_imbalance() -> f64 {
    30.0
}
fn default_mm_order_size() -> f64 {
    3.0
}
fn default_mm_leverage() -> u32 {
    2
}
fn default_mm_wake_interval() -> u64 {
    500
}
fn default_mm_balance() -> i128 {
    1_000_000_000_000 // $1M
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimConfig {
    scenario_name: String,
    duration_sec: u64,
    logs_dir: String,
    exchange: ExchangeConfig,
    oracles: Vec<OracleConfig>,
    #[serde(default)]
    smart_traders: Vec<SmartTraderJsonConfig>,
    #[serde(default)]
    limit_traders: Vec<LimitTraderJsonConfig>,
    #[serde(default)]
    liquidation_agent: Option<LiquidationAgentConfig>,
    #[serde(default)]
    market_maker: Option<MarketMakerJsonConfig>,
    #[serde(default)]
    keepers: Vec<KeeperJsonConfig>,
}

fn default_wake_interval() -> u64 {
    3000
}

impl SimConfig {
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            scenario_name: "simple_demo".to_string(),
            duration_sec: 10,
            logs_dir: "logs".to_string(),
            exchange: ExchangeConfig {
                id: 1,
                name: "PerpExchange".to_string(),
                markets: vec![MarketJsonConfig {
                    id: 0,
                    symbol: "ETH-USD".to_string(),
                    index_token: "ETH".to_string(),
                    index_decimals: 18,
                    collateral_token: "USDT".to_string(),
                    collateral_decimals: 6,
                    initial_liquidity: LiquidityConfig {
                        collateral_amount: 1_000_000_000_000,
                        index_amount: 500_000_000_000,
                        liquidity_usd: 2_000_000_000_000,
                    },
                }],
            },
            oracles: vec![OracleConfig {
                id: 2,
                name: "PythOracle".to_string(),
                symbols: vec!["ETH-USD".to_string(), "USDT-USD".to_string()],
                provider: "Pyth".to_string(),
                cache_duration_ms: 10000,
                wake_interval_ms: 3000,
            }],
            smart_traders: vec![],
            limit_traders: vec![],
            liquidation_agent: None,
            market_maker: None,
            keepers: vec![],
        }
    }
}

/// Helper function to parse strategy from JSON config
fn parse_strategy(smart_cfg: &SmartTraderJsonConfig) -> TradingStrategy {
    let side = match smart_cfg.side.to_lowercase().as_str() {
        "short" | "sell" => Side::Sell,
        _ => Side::Buy,
    };

    match smart_cfg.strategy.to_lowercase().as_str() {
        "hodler" => TradingStrategy::Hodler {
            side,
            hold_duration_sec: smart_cfg.hold_duration_sec,
            leverage: smart_cfg.leverage,
            take_profit_pct: smart_cfg.take_profit_pct,
            stop_loss_pct: smart_cfg.stop_loss_pct,
        },
        "institutional" | "inst" => TradingStrategy::Institutional {
            side,
            leverage: smart_cfg.leverage,
            take_profit_pct: smart_cfg.take_profit_pct.unwrap_or(3.0),
            stop_loss_pct: smart_cfg.stop_loss_pct.unwrap_or(8.0),
            min_hold_sec: smart_cfg.min_hold_sec.unwrap_or(60),
            max_hold_sec: smart_cfg.max_hold_sec.unwrap_or(600),
            reentry_delay_sec: smart_cfg.reentry_delay_sec.unwrap_or(30),
        },
        "trend_follower" | "trend" => TradingStrategy::TrendFollower {
            lookback_sec: smart_cfg.lookback_sec,
            threshold_pct: smart_cfg.threshold_pct,
            leverage: smart_cfg.leverage,
            take_profit_pct: smart_cfg.take_profit_pct,
            stop_loss_pct: smart_cfg.stop_loss_pct,
        },
        "mean_reversion" | "meanrev" | "mr" => TradingStrategy::MeanReversion {
            lookback_periods: smart_cfg.lookback_periods.unwrap_or(20),
            entry_deviation_pct: smart_cfg.entry_deviation_pct.unwrap_or(1.0),
            exit_deviation_pct: smart_cfg.exit_deviation_pct.unwrap_or(0.2),
            leverage: smart_cfg.leverage,
            max_hold_sec: smart_cfg.max_hold_sec.unwrap_or(300),
        },
        "arbitrageur" | "arb" => TradingStrategy::Arbitrageur {
            min_imbalance_pct: smart_cfg.entry_deviation_pct.unwrap_or(5.0), // reuse field
            leverage: smart_cfg.leverage,
            hold_duration_sec: smart_cfg.hold_duration_sec,
            take_profit_pct: smart_cfg.take_profit_pct,
            stop_loss_pct: smart_cfg.stop_loss_pct,
        },
        "funding_harvester" | "funding" | "fh" => TradingStrategy::FundingHarvester {
            min_imbalance_pct: smart_cfg.entry_deviation_pct.unwrap_or(3.0),
            leverage: smart_cfg.leverage,
            min_hold_sec: smart_cfg.min_hold_sec.unwrap_or(60),
            max_hold_sec: smart_cfg.max_hold_sec.unwrap_or(600),
            exit_imbalance_pct: smart_cfg.exit_deviation_pct.unwrap_or(5.0),
            stop_loss_pct: smart_cfg.stop_loss_pct.unwrap_or(10.0),
        },
        _ => {
            eprintln!("[Scenario] Unknown strategy: {}, using Hodler", smart_cfg.strategy);
            TradingStrategy::Hodler {
                side,
                hold_duration_sec: smart_cfg.hold_duration_sec,
                leverage: smart_cfg.leverage,
                take_profit_pct: None,
                stop_loss_pct: None,
            }
        }
    }
}

/// Helper function to create SmartTraderAgent from JSON config
fn create_smart_trader(smart_cfg: &SmartTraderJsonConfig, exchange_id: u32) -> SmartTraderAgent {
    let strategy = parse_strategy(smart_cfg);

    let qty_min = smart_cfg.qty_min.unwrap_or(smart_cfg.qty as f64);
    let qty_max = smart_cfg.qty_max.unwrap_or(smart_cfg.qty as f64);

    let smart_config = SmartTraderConfig {
        name: smart_cfg.name.clone(),
        exchange_id,
        symbol: smart_cfg.symbol.clone(),
        address: None,
        strategy,
        qty_min,
        qty_max,
        wake_interval_ms: smart_cfg.wake_interval_ms,
        balance: smart_cfg.balance,
        start_delay_ms: smart_cfg.start_delay_ms,
    };

    SmartTraderAgent::new(smart_cfg.id, smart_config)
}

fn parse_limit_strategy(cfg: &LimitTraderJsonConfig) -> LimitStrategy {
    match cfg.strategy.to_lowercase().as_str() {
        "mean_reversion" | "meanrev" => LimitStrategy::MeanReversion {
            entry_offset_pct: cfg.entry_offset_pct.unwrap_or(1.5),
            stop_loss_pct: cfg.stop_loss_pct.unwrap_or(3.0),
            take_profit_pct: cfg.take_profit_pct.unwrap_or(2.0),
            leverage: cfg.leverage,
            trend_lookback: cfg.trend_lookback.unwrap_or(10),
        },
        "breakout" => {
            let direction = match cfg.direction.as_deref() {
                Some("sell") | Some("short") => Side::Sell,
                _ => Side::Buy,
            };
            LimitStrategy::Breakout {
                breakout_offset_pct: cfg.breakout_offset_pct.unwrap_or(1.0),
                stop_loss_pct: cfg.stop_loss_pct.unwrap_or(2.0),
                take_profit_pct: cfg.take_profit_pct.unwrap_or(3.0),
                leverage: cfg.leverage,
                direction,
            }
        }
        "grid" => LimitStrategy::Grid {
            levels: 5,
            spacing_pct: 1.0,
            qty_per_level: cfg.qty,
            leverage: cfg.leverage,
            take_profit_pct: cfg.take_profit_pct.unwrap_or(1.5),
        },
        "smart" => {
            let order_mode = match cfg.order_mode.as_deref() {
                Some("active") => OrderMode::Active,
                _ => OrderMode::Passive,
            };
            LimitStrategy::Smart {
                sma_fast: cfg.sma_fast.unwrap_or(10),
                sma_slow: cfg.sma_slow.unwrap_or(30),
                rsi_period: cfg.rsi_period.unwrap_or(14),
                rsi_low: cfg.rsi_low.unwrap_or(35.0),
                rsi_high: cfg.rsi_high.unwrap_or(65.0),
                atr_period: cfg.atr_period.unwrap_or(14),
                entry_atr_mult: cfg.entry_atr_mult.unwrap_or(0.5),
                stop_atr_mult: cfg.stop_atr_mult.unwrap_or(1.2),
                take_atr_mult: cfg.take_atr_mult.unwrap_or(1.8),
                leverage: cfg.leverage,
                order_mode,
            }
        }
        _ => LimitStrategy::MeanReversion {
            entry_offset_pct: 1.5,
            stop_loss_pct: 3.0,
            take_profit_pct: 2.0,
            leverage: cfg.leverage,
            trend_lookback: 10,
        },
    }
}

fn create_limit_trader(cfg: &LimitTraderJsonConfig, exchange_id: u32) -> LimitTraderAgent {
    let strategy = parse_limit_strategy(cfg);

    let config = LimitTraderConfig {
        name: cfg.name.clone(),
        exchange_id,
        symbol: cfg.symbol.clone(),
        address: None,
        strategy,
        qty: cfg.qty,
        wake_interval_ms: cfg.wake_interval_ms,
        balance: cfg.balance,
    };

    LimitTraderAgent::new(cfg.id, config)
}

const DEFAULT_DEPOSIT_MICRO_USD: i128 = 1_000_000_000_000; // $1M

fn balance_to_collateral_tokens(balance_micro_usd: i128, collateral_decimals: u32) -> U256 {
    if balance_micro_usd <= 0 {
        return U256::zero();
    }
    let mut amount = U256::from(balance_micro_usd as u128);
    if collateral_decimals > 6 {
        let exp = (collateral_decimals - 6) as usize;
        amount *= U256::exp10(exp);
    } else if collateral_decimals < 6 {
        let exp = (6 - collateral_decimals) as usize;
        let divisor = U256::exp10(exp);
        if !divisor.is_zero() {
            amount /= divisor;
        }
    }
    amount
}

fn deposit_initial_balances(config: &SimConfig, vara_client: &VaraClient) {
    let collateral_decimals = config
        .exchange
        .markets
        .first()
        .map(|m| m.collateral_decimals)
        .unwrap_or(6);

    let mut deposits: Vec<(u32, i128)> = Vec::new();

    if let Some(mm_cfg) = &config.market_maker {
        deposits.push((mm_cfg.id, mm_cfg.balance));
    }

    for smart_cfg in &config.smart_traders {
        let balance = smart_cfg.balance.unwrap_or(DEFAULT_DEPOSIT_MICRO_USD);
        deposits.push((smart_cfg.id, balance));
    }

    for limit_cfg in &config.limit_traders {
        let balance = limit_cfg.balance.unwrap_or(DEFAULT_DEPOSIT_MICRO_USD);
        deposits.push((limit_cfg.id, balance));
    }

    if deposits.is_empty() {
        println!("[Scenario] No deposits to make");
        return;
    }

    println!(
        "[Scenario] Making {} initial deposits in parallel (collateral_decimals={})",
        deposits.len(),
        collateral_decimals
    );

    // Convert to (agent_id, U256 amount) pairs for batch call
    let batch: Vec<(u32, primitive_types::U256)> = deposits
        .iter()
        .map(|(agent_id, balance_micro)| {
            (*agent_id, balance_to_collateral_tokens(*balance_micro, collateral_decimals))
        })
        .filter(|(_, amount)| !amount.is_zero())
        .collect();

    match vara_client.deposit_batch(&batch) {
        Ok((success, failed)) => {
            println!(
                "[Scenario] Deposits completed: {}/{} successful, {} failed",
                success,
                batch.len(),
                failed
            );
        }
        Err(e) => {
            eprintln!("[Scenario] Deposit batch error: {}", e);
        }
    }
}

/// Register all CSV event loggers on the engine.
fn register_csv_loggers(engine: &mut SimEngine, logs_dir: &str) {
    let _ = fs::create_dir_all(logs_dir);
    if let Ok(l) = CsvOracleLogger::new(logs_dir) { engine.kernel.event_bus_mut().subscribe(Box::new(l)); }
    if let Ok(l) = CsvExecutionLogger::new(logs_dir) { engine.kernel.event_bus_mut().subscribe(Box::new(l)); }
    if let Ok(l) = CsvPositionLogger::new(logs_dir) { engine.kernel.event_bus_mut().subscribe(Box::new(l)); }
    if let Ok(l) = CsvMarketLogger::new(logs_dir) { engine.kernel.event_bus_mut().subscribe(Box::new(l)); }
    if let Ok(l) = CsvLiquidationLogger::new(logs_dir) { engine.kernel.event_bus_mut().subscribe(Box::new(l)); }
}

/// Convert JSON market configs to ExchangeAgent MarketConfig.
fn convert_markets(json_markets: &[MarketJsonConfig]) -> Vec<MarketConfig> {
    json_markets
        .iter()
        .map(|m| MarketConfig {
            id: m.id,
            symbol: m.symbol.clone(),
            index_token: m.index_token.clone(),
            collateral_token: m.collateral_token.clone(),
            collateral_amount: m.initial_liquidity.collateral_amount,
            index_amount: m.initial_liquidity.index_amount,
            liquidity_usd: m.initial_liquidity.liquidity_usd,
            index_decimals: m.index_decimals,
            collateral_decimals: m.collateral_decimals,
        })
        .collect()
}

/// Register the orders.csv event listener on the engine.
fn register_orders_csv(engine: &mut SimEngine, logs_dir: &str) {
    let orders_path = format!("{}/orders.csv", logs_dir);
    let file = match File::create(&orders_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[Scenario] Failed to create {}: {}", orders_path, e);
            return;
        }
    };
    let writer = RefCell::new(BufWriter::new(file));
    if let Err(e) = writeln!(writer.borrow_mut(), "ts,from,to,msg_type,symbol,side,price,qty,reason") {
        eprintln!("[Scenario] Failed to write CSV header: {}", e);
        return;
    }

    let listener = move |ev: &SimEvent| {
        if let SimEvent::OrderLog { ts, from, to, msg_type, symbol, side, price, qty } = ev {
            let symbol_str = symbol.as_deref().unwrap_or("");
            let side_str = side.map(|s| format!("{:?}", s)).unwrap_or_default();
            let price_str = price.map(|p| p.to_string()).unwrap_or_default();
            let qty_str = qty.map(|q| q.to_string()).unwrap_or_default();
            if let Err(e) = writeln!(
                writer.borrow_mut(),
                "{ts},{from},{to},{:?},{symbol},{side},{price},{qty}",
                msg_type,
                symbol = symbol_str,
                side = side_str,
                price = price_str,
                qty = qty_str,
            ) {
                eprintln!("[Scenario] failed to write to orders.csv: {e}");
            }
        }
    };
    engine.kernel.event_bus_mut().subscribe(Box::new(ClosureListener { closure: listener }));
}

/// Run a simulation with given configuration
fn run_with_config(config: SimConfig, skip_deposits: bool, vara_client: Arc<VaraClient>) {
    println!("[Scenario] Loading scenario: {}", config.scenario_name);
    println!("[Scenario] Duration: {}s", config.duration_sec);
    println!("[Scenario] Markets: {}", config.exchange.markets.len());
    println!("[Scenario] Oracles: {}", config.oracles.len());
    println!("[Scenario] SmartTraders: {}", config.smart_traders.len());
    println!("[Scenario] Blockchain: Vara Network");

    let max_ticks = (config.duration_sec * 1000 / 100) as usize;

    let mut engine = SimEngine::with_default_latency();
    let address_book = AddressBook::load();
    address_book.write_funding_list("keys/funding_addresses.txt");
    if skip_deposits {
        println!("[Scenario] Skipping initial deposits (flag enabled)");
    } else {
        deposit_initial_balances(&config, &vara_client);
    }

    register_csv_loggers(&mut engine, &config.logs_dir);
    register_orders_csv(&mut engine, &config.logs_dir);

    let markets = convert_markets(&config.exchange.markets);
    let tx_result_rx = vara_client.take_tx_result_receiver();
    let exchange = ExchangeAgent::new(
        config.exchange.id,
        config.exchange.name.clone(),
        markets,
        vara_client.clone(),
        tx_result_rx,
        Some(&config.logs_dir),
    );
    engine.kernel.add_agent(Box::new(exchange));

    for oracle_cfg in &config.oracles {
        let cache_duration_sec = oracle_cfg.cache_duration_ms / 1000;

        let provider: Box<dyn crate::api::PriceProvider> = match oracle_cfg.provider.as_str() {
            "Pyth" => {
                let pyth = PythProvider::new();
                Box::new(CachedPriceProvider::new(pyth, cache_duration_sec))
            }
            _ => {
                eprintln!("[Scenario] Unknown provider: {}, using Pyth", oracle_cfg.provider);
                let pyth = PythProvider::new();
                Box::new(CachedPriceProvider::new(pyth, cache_duration_sec))
            }
        };

        let wake_interval_ns = oracle_cfg.wake_interval_ms * 1_000_000;

        engine.kernel.add_agent(Box::new(OracleAgent::new(
            oracle_cfg.id,
            oracle_cfg.name.clone(),
            oracle_cfg.symbols.clone(),
            config.exchange.id,
            wake_interval_ns,
            provider,
        )));
    }

    // Add Market Maker if configured (MUST be added BEFORE other traders for seed liquidity)
    if let Some(mm_cfg) = &config.market_maker {
        let mm_address = address_book.address_for_agent(mm_cfg.id).unwrap_or_else(|| {
            eprintln!("[Scenario] Missing address for MarketMaker id={}", mm_cfg.id);
            std::process::exit(1);
        });
        let mm_config = MarketMakerConfig {
            name: mm_cfg.name.clone(),
            exchange_id: config.exchange.id,
            symbol: mm_cfg.symbol.clone(),
            address: Some(mm_address),
            target_oi_per_side: mm_cfg.target_oi_per_side,
            max_imbalance_pct: mm_cfg.max_imbalance_pct,
            order_size_tokens: mm_cfg.order_size_tokens,
            leverage: mm_cfg.leverage,
            wake_interval_ms: mm_cfg.wake_interval_ms,
            balance: mm_cfg.balance,
        };
        engine
            .kernel
            .add_agent(Box::new(MarketMakerAgent::new(mm_cfg.id, mm_config)));
        println!("[Scenario] Added MarketMaker: {}", mm_cfg.name);
    }

    // Create smart traders
    for smart_cfg in &config.smart_traders {
        let address = address_book.address_for_agent(smart_cfg.id).unwrap_or_else(|| {
            eprintln!("[Scenario] Missing address for SmartTrader id={}", smart_cfg.id);
            std::process::exit(1);
        });
        let mut agent = create_smart_trader(smart_cfg, config.exchange.id);
        agent.set_address(address);
        engine.kernel.add_agent(Box::new(agent));
    }

    // Create limit traders
    for limit_cfg in &config.limit_traders {
        let address = address_book.address_for_agent(limit_cfg.id).unwrap_or_else(|| {
            eprintln!("[Scenario] Missing address for LimitTrader id={}", limit_cfg.id);
            std::process::exit(1);
        });
        let mut agent = create_limit_trader(limit_cfg, config.exchange.id);
        agent.set_address(address);
        engine.kernel.add_agent(Box::new(agent));
        println!("[Scenario] Added LimitTrader: {}", limit_cfg.name);
    }

    // Add keepers (no blockchain access - they send messages to Exchange)
    for keeper_cfg in &config.keepers {
        let keeper_config = KeeperConfig {
            name: keeper_cfg.name.clone(),
            exchange_id: config.exchange.id,
            address: None, // Keepers don't need address - Exchange signs for them
            wake_interval_ms: keeper_cfg.wake_interval_ms,
        };
        let agent = KeeperAgent::new(keeper_cfg.id, keeper_config);
        engine.kernel.add_agent(Box::new(agent));
        println!("[Scenario] Added Keeper: {}", keeper_cfg.name);
    }

    // Add liquidation agent if configured (no blockchain access - sends LiquidationScan to Exchange)
    if let Some(liq_cfg) = &config.liquidation_agent {
        let wake_interval_ns = liq_cfg.wake_interval_ms * 1_000_000;
        let agent = LiquidationAgent::new(liq_cfg.id, liq_cfg.name.clone(), config.exchange.id, wake_interval_ns);
        engine.kernel.add_agent(Box::new(agent));
        println!("[Scenario] Added Liquidation: {}", liq_cfg.name);
    }

    println!("[Scenario] starting {}", config.scenario_name);
    engine.run(max_ticks);
    println!("[Scenario] finished {}", config.scenario_name);
}

fn find_config_file(scenario_name: &str) -> Option<String> {
    // Try multiple possible locations
    let candidates = [
        format!("sim-engine/src/scenarios/{}.json", scenario_name),
        format!("src/scenarios/{}.json", scenario_name),
        format!("scenarios/{}.json", scenario_name),
        format!("{}.json", scenario_name),
    ];

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.clone());
        }
    }
    None
}

/// Run scenario with blockchain
pub fn run_scenario_with_blockchain(
    scenario_name: &str,
    skip_deposits: bool,
    vara_client: Arc<VaraClient>,
) {
    let config = match find_config_file(scenario_name) {
        Some(path) => {
            println!("[Scenario] Found config: {}", path);
            SimConfig::from_file(&path).unwrap_or_else(|e| {
                eprintln!("[Scenario] Failed to parse {}: {}", path, e);
                eprintln!("[Scenario] Using default configuration");
                SimConfig::default()
            })
        }
        None => {
            eprintln!("[Scenario] Config file not found for: {}", scenario_name);
            eprintln!("[Scenario] Using default configuration");
            SimConfig::default()
        }
    };

    run_with_config(config, skip_deposits, vara_client);
}

/// Run simulation in realtime mode with blockchain
pub fn run_realtime_with_blockchain(
    scenario_name: &str,
    tick_ms: u64,
    api_port: u16,
    skip_deposits: bool,
    vara_client: Arc<VaraClient>,
) {
    let config = match find_config_file(scenario_name) {
        Some(path) => {
            println!("[Scenario] Found config: {}", path);
            SimConfig::from_file(&path).unwrap_or_else(|e| {
                eprintln!("[Scenario] Failed to parse {}: {}", path, e);
                SimConfig::default()
            })
        }
        None => {
            eprintln!("[Scenario] Config file not found for: {}", scenario_name);
            SimConfig::default()
        }
    };

    run_realtime_with_config(config, tick_ms, api_port, skip_deposits, vara_client);
}

/// Internal realtime runner with config
fn run_realtime_with_config(
    config: SimConfig,
    tick_ms: u64,
    api_port: u16,
    skip_deposits: bool,
    vara_client: Arc<VaraClient>,
) {
    println!("[Scenario] Loading: {} (REALTIME)", config.scenario_name);
    println!("[Scenario] Tick: {}ms, API port: {}", tick_ms, api_port);

    let max_ticks = usize::MAX; // Run indefinitely

    let mut engine = SimEngine::with_realtime(tick_ms);
    let address_book = AddressBook::load();
    address_book.write_funding_list("keys/funding_addresses.txt");
    if skip_deposits {
        println!("[Scenario] Skipping initial deposits (flag enabled)");
    } else {
        deposit_initial_balances(&config, &vara_client);
    }

    register_csv_loggers(&mut engine, &config.logs_dir);

    // Start API server (HTTP)
    let (response_tx, response_rx) = crossbeam_channel::unbounded();
    let (response_tx_ws, response_rx_ws) = crossbeam_channel::unbounded();

    // Use a shared channel for commands from both HTTP and WS
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();

    // Start HTTP API
    let _api_server = crate::api::ApiServer::start_with_channel(api_port, response_rx, cmd_tx.clone());

    // Start WebSocket API (on port + 1)
    let ws_port = api_port + 1;
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let _ws_server = crate::api::WsServer::start(ws_port, cmd_tx, event_rx, response_rx_ws);

    // Subscribe WS to all events
    {
        let event_tx = event_tx.clone();
        let listener = move |ev: &SimEvent| {
            let _ = event_tx.send(ev.clone());
        };
        engine
            .kernel
            .event_bus_mut()
            .subscribe(Box::new(ClosureListener { closure: listener }));
    }

    let (human_response_tx, human_response_rx) = crossbeam_channel::unbounded::<crate::api::ApiResponse>();

    // Response forwarder thread
    std::thread::spawn(move || {
        while let Ok(resp) = human_response_rx.recv() {
            let _ = response_tx.send(resp.clone());
            let _ = response_tx_ws.send(resp);
        }
    });

    let markets = convert_markets(&config.exchange.markets);
    let tx_result_rx = vara_client.take_tx_result_receiver();
    let exchange = ExchangeAgent::new(
        config.exchange.id,
        config.exchange.name.clone(),
        markets,
        vara_client.clone(),
        tx_result_rx,
        Some(&config.logs_dir),
    );
    engine.kernel.add_agent(Box::new(exchange));

    // Add oracles
    for oracle_cfg in &config.oracles {
        let provider: Box<dyn crate::api::PriceProvider> = {
            let pyth = PythProvider::new();
            Box::new(CachedPriceProvider::new(pyth, oracle_cfg.cache_duration_ms / 1000))
        };

        engine.kernel.add_agent(Box::new(OracleAgent::new(
            oracle_cfg.id,
            oracle_cfg.name.clone(),
            oracle_cfg.symbols.clone(),
            config.exchange.id,
            oracle_cfg.wake_interval_ms * 1_000_000,
            provider,
        )));
    }

    // Add Market Maker if configured (MUST be added BEFORE other traders for seed liquidity)
    if let Some(mm_cfg) = &config.market_maker {
        let mm_address = address_book.address_for_agent(mm_cfg.id).unwrap_or_else(|| {
            eprintln!("[Scenario] Missing address for MarketMaker id={}", mm_cfg.id);
            std::process::exit(1);
        });
        let mm_config = MarketMakerConfig {
            name: mm_cfg.name.clone(),
            exchange_id: config.exchange.id,
            symbol: mm_cfg.symbol.clone(),
            address: Some(mm_address),
            target_oi_per_side: mm_cfg.target_oi_per_side,
            max_imbalance_pct: mm_cfg.max_imbalance_pct,
            order_size_tokens: mm_cfg.order_size_tokens,
            leverage: mm_cfg.leverage,
            wake_interval_ms: mm_cfg.wake_interval_ms,
            balance: mm_cfg.balance,
        };
        engine
            .kernel
            .add_agent(Box::new(MarketMakerAgent::new(mm_cfg.id, mm_config)));
        println!("[Scenario] Added MarketMaker: {}", mm_cfg.name);
    }

    // Add smart traders
    for smart_cfg in &config.smart_traders {
        let address = address_book.address_for_agent(smart_cfg.id).unwrap_or_else(|| {
            eprintln!("[Scenario] Missing address for SmartTrader id={}", smart_cfg.id);
            std::process::exit(1);
        });
        let mut agent = create_smart_trader(smart_cfg, config.exchange.id);
        agent.set_address(address);
        engine.kernel.add_agent(Box::new(agent));
    }

    // Add limit traders
    for limit_cfg in &config.limit_traders {
        let address = address_book.address_for_agent(limit_cfg.id).unwrap_or_else(|| {
            eprintln!("[Scenario] Missing address for LimitTrader id={}", limit_cfg.id);
            std::process::exit(1);
        });
        let mut agent = create_limit_trader(limit_cfg, config.exchange.id);
        agent.set_address(address);
        engine.kernel.add_agent(Box::new(agent));
    }

    // Add keepers (no blockchain access - they send messages to Exchange)
    for keeper_cfg in &config.keepers {
        let keeper_config = KeeperConfig {
            name: keeper_cfg.name.clone(),
            exchange_id: config.exchange.id,
            address: None, // Keepers don't need address - Exchange signs for them
            wake_interval_ms: keeper_cfg.wake_interval_ms,
        };
        let agent = KeeperAgent::new(keeper_cfg.id, keeper_config);
        engine.kernel.add_agent(Box::new(agent));
    }

    // Add liquidation agent if configured (no blockchain access - sends LiquidationScan to Exchange)
    if let Some(liq_cfg) = &config.liquidation_agent {
        let wake_interval_ns = liq_cfg.wake_interval_ms * 1_000_000;
        let agent = LiquidationAgent::new(liq_cfg.id, liq_cfg.name.clone(), config.exchange.id, wake_interval_ns);
        engine.kernel.add_agent(Box::new(agent));
    }

    // Add HumanAgent (id=100, reserved). Default to bot_100 address if not provided.
    let human_address = std::env::var("VARA_HUMAN_ADDRESS")
        .ok()
        .or_else(|| address_book.address_for_agent(100));
    engine.kernel.add_agent(Box::new(HumanAgent::new(
        100,
        "HumanTrader".to_string(),
        config.exchange.id,
        human_address,
        cmd_rx,
        human_response_tx,
        tick_ms,
    )));

    println!();
    println!("=== REALTIME MODE ===");
    println!(
        "Agents: {} smart + {} limit + {} keepers + HumanAgent{}{}",
        config.smart_traders.len(),
        config.limit_traders.len(),
        config.keepers.len(),
        if config.market_maker.is_some() { " + MM" } else { "" },
        if config.liquidation_agent.is_some() {
            " + Liquidator"
        } else {
            ""
        }
    );
    println!();
    println!("=== API Endpoints ===");
    println!("  POST http://localhost:{}/order", api_port);
    println!("  WS   ws://localhost:{}", ws_port);
    println!("       {{\"action\":\"open\", \"symbol\":\"ETH-USD\", ...}}");
    println!();
    println!("Press Ctrl+C to stop");
    println!();

    engine.run(max_ticks);
}
