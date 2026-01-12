use crate::agents::{
    exchange_agent::{ExchangeAgent, MarketConfig},
    human_agent::HumanAgent,
    liquidation_agent::LiquidationAgent,
    oracle_agent::OracleAgent,
    smart_trader_agent::{SmartTraderAgent, SmartTraderConfig, TradingStrategy},
    trader_agent::TraderAgent,
};
use crate::api::{CachedPriceProvider, PythProvider};
use crate::events::{EventListener, SimEvent};
use crate::logging::{
    CsvExecutionLogger, CsvLiquidationLogger, CsvMarketLogger, CsvOracleLogger, CsvPositionLogger,
};
use crate::messages::Side;
use crate::sim_engine::SimEngine;
use crossbeam_channel;

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{BufWriter, Write};

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
struct TraderConfig {
    id: u32,
    name: String,
    symbol: String,
    #[serde(default = "default_trader_wake_interval")]
    wake_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SmartTraderJsonConfig {
    id: u32,
    name: String,
    symbol: String,
    strategy: String, // "hodler", "risky", "trend_follower"
    #[serde(default = "default_side")]
    side: String, // "long" or "short" (for hodler)
    #[serde(default = "default_leverage")]
    leverage: u32,
    #[serde(default = "default_qty")]
    qty: u64, // legacy: if qty_min/qty_max not set, use this for both
    #[serde(default)]
    qty_min: Option<u64>, // min tokens to trade (random range)
    #[serde(default)]
    qty_max: Option<u64>, // max tokens to trade (random range)
    #[serde(default = "default_hold_duration")]
    hold_duration_sec: u64, // for hodler
    #[serde(default = "default_lookback")]
    lookback_sec: u64, // for trend_follower
    #[serde(default = "default_threshold")]
    threshold_pct: f64, // for trend_follower
    #[serde(default = "default_smart_wake_interval")]
    wake_interval_ms: u64,
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
pub struct SimConfig {
    scenario_name: String,
    duration_sec: u64,
    logs_dir: String,
    exchange: ExchangeConfig,
    oracles: Vec<OracleConfig>,
    #[serde(default)]
    traders: Vec<TraderConfig>,
    #[serde(default)]
    smart_traders: Vec<SmartTraderJsonConfig>,
    #[serde(default)]
    liquidation_agent: Option<LiquidationAgentConfig>,
}

fn default_wake_interval() -> u64 {
    3000
}

fn default_trader_wake_interval() -> u64 {
    2000
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
            traders: vec![TraderConfig {
                id: 3,
                name: "Trader1".to_string(),
                symbol: "ETH-USD".to_string(),
                wake_interval_ms: 2000,
            }],
            smart_traders: vec![],
            liquidation_agent: None,
        }
    }
}

/// Run a simulation with given configuration
fn run_with_config(config: SimConfig) {
    println!("[Scenario] Loading scenario: {}", config.scenario_name);
    println!("[Scenario] Duration: {}s", config.duration_sec);
    println!("[Scenario] Markets: {}", config.exchange.markets.len());
    println!("[Scenario] Oracles: {}", config.oracles.len());
    println!("[Scenario] Traders: {}", config.traders.len());
    println!("[Scenario] SmartTraders: {}", config.smart_traders.len());

    let max_ticks = (config.duration_sec * 1000 / 100) as usize;

    let mut engine = SimEngine::with_default_latency();

    // Register CSV loggers for all event types
    let _ = fs::create_dir_all(&config.logs_dir);
    
    if let Ok(logger) = CsvOracleLogger::new(&config.logs_dir) {
        engine.kernel.event_bus_mut().subscribe(Box::new(logger));
    }
    if let Ok(logger) = CsvExecutionLogger::new(&config.logs_dir) {
        engine.kernel.event_bus_mut().subscribe(Box::new(logger));
    }
    if let Ok(logger) = CsvPositionLogger::new(&config.logs_dir) {
        engine.kernel.event_bus_mut().subscribe(Box::new(logger));
    }
    if let Ok(logger) = CsvMarketLogger::new(&config.logs_dir) {
        engine.kernel.event_bus_mut().subscribe(Box::new(logger));
    }
    if let Ok(logger) = CsvLiquidationLogger::new(&config.logs_dir) {
        engine.kernel.event_bus_mut().subscribe(Box::new(logger));
    }

    {
        let orders_path = format!("{}/orders.csv", config.logs_dir);
        let file = File::create(&orders_path).expect("cannot create orders.csv");
        let writer = RefCell::new(BufWriter::new(file));

        writeln!(writer.borrow_mut(), "ts,from,to,msg_type,symbol,side,price,qty,reason")
            .expect("cannot write CSV header");

        let listener = move |ev: &SimEvent| {
            if let SimEvent::OrderLog {
                ts,
                from,
                to,
                msg_type,
                symbol,
                side,
                price,
                qty,
            } = ev
            {
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

        engine
            .kernel
            .event_bus_mut()
            .subscribe(Box::new(ClosureListener { closure: listener }));
    }

    // Convert JSON market configs to ExchangeAgent MarketConfig
    let markets: Vec<MarketConfig> = config
        .exchange
        .markets
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
        .collect();

    engine.kernel.add_agent(Box::new(ExchangeAgent::new(
        config.exchange.id,
        config.exchange.name.clone(),
        markets,
    )));

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

    for trader_cfg in &config.traders {
        engine.kernel.add_agent(Box::new(TraderAgent::new(
            trader_cfg.id,
            trader_cfg.name.clone(),
            config.exchange.id,
            trader_cfg.symbol.clone(),
        )));
    }

    // Create smart traders
    for smart_cfg in &config.smart_traders {
        let side = match smart_cfg.side.to_lowercase().as_str() {
            "short" | "sell" => Side::Sell,
            _ => Side::Buy,
        };

        let strategy = match smart_cfg.strategy.to_lowercase().as_str() {
            "hodler" => TradingStrategy::Hodler {
                side,
                hold_duration_sec: smart_cfg.hold_duration_sec,
                leverage: smart_cfg.leverage,
            },
            "risky" => TradingStrategy::Risky {
                leverage: smart_cfg.leverage,
            },
            "trend_follower" | "trend" => TradingStrategy::TrendFollower {
                lookback_sec: smart_cfg.lookback_sec,
                threshold_pct: smart_cfg.threshold_pct,
                leverage: smart_cfg.leverage,
            },
            _ => {
                eprintln!("[Scenario] Unknown strategy: {}, using Risky", smart_cfg.strategy);
                TradingStrategy::Risky {
                    leverage: smart_cfg.leverage,
                }
            }
        };

        // Support both legacy qty and new qty_min/qty_max
        let qty_min = smart_cfg.qty_min.unwrap_or(smart_cfg.qty);
        let qty_max = smart_cfg.qty_max.unwrap_or(smart_cfg.qty);

        let smart_config = SmartTraderConfig {
            name: smart_cfg.name.clone(),
            exchange_id: config.exchange.id,
            symbol: smart_cfg.symbol.clone(),
            strategy,
            qty_min,
            qty_max,
            wake_interval_ms: smart_cfg.wake_interval_ms,
        };

        engine
            .kernel
            .add_agent(Box::new(SmartTraderAgent::new(smart_cfg.id, smart_config)));
    }

    // Add liquidation agent if configured
    if let Some(liq_cfg) = &config.liquidation_agent {
        let wake_interval_ns = liq_cfg.wake_interval_ms * 1_000_000;
        engine.kernel.add_agent(Box::new(LiquidationAgent::new(
            liq_cfg.id,
            liq_cfg.name.clone(),
            config.exchange.id,
            wake_interval_ns,
        )));
    }

    println!("[Scenario] starting {}", config.scenario_name);
    engine.run(max_ticks);
    println!("[Scenario] finished {}", config.scenario_name);
}

pub fn run() {
    run_scenario("simple_demo");
}

pub fn run_scenario(scenario_name: &str) {
    let config_path = format!("sim-engine/src/scenarios/{}.json", scenario_name);

    let config = SimConfig::from_file(&config_path).unwrap_or_else(|e| {
        eprintln!("[Scenario] Failed to load {}: {}", config_path, e);
        eprintln!("[Scenario] Using default configuration");
        SimConfig::default()
    });

    run_with_config(config);
}

/// Run simulation in realtime mode with HTTP API for HumanAgent
pub fn run_realtime(scenario_name: &str, tick_ms: u64, api_port: u16) {
    let config_path = format!("sim-engine/src/scenarios/{}.json", scenario_name);

    let config = SimConfig::from_file(&config_path).unwrap_or_else(|e| {
        eprintln!("[Scenario] Failed to load {}: {}", config_path, e);
        SimConfig::default()
    });

    println!("[Scenario] Loading: {} (REALTIME)", config.scenario_name);
    println!("[Scenario] Tick: {}ms, API port: {}", tick_ms, api_port);

    let max_ticks = usize::MAX; // Run indefinitely

    let mut engine = SimEngine::with_realtime(tick_ms);

    // Register CSV loggers for all event types
    let _ = fs::create_dir_all(&config.logs_dir);
    
    if let Ok(logger) = CsvOracleLogger::new(&config.logs_dir) {
        engine.kernel.event_bus_mut().subscribe(Box::new(logger));
    }
    if let Ok(logger) = CsvExecutionLogger::new(&config.logs_dir) {
        engine.kernel.event_bus_mut().subscribe(Box::new(logger));
    }
    if let Ok(logger) = CsvPositionLogger::new(&config.logs_dir) {
        engine.kernel.event_bus_mut().subscribe(Box::new(logger));
    }
    if let Ok(logger) = CsvMarketLogger::new(&config.logs_dir) {
        engine.kernel.event_bus_mut().subscribe(Box::new(logger));
    }
    if let Ok(logger) = CsvLiquidationLogger::new(&config.logs_dir) {
        engine.kernel.event_bus_mut().subscribe(Box::new(logger));
    }

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

    // We need to split responses to both HTTP and WS?
    // HumanAgent has only one response_tx.
    // Solution: Create a "Splitter" channel?
    // Or simpler: HumanAgent sends to a channel, and we have a thread that forwards to both HTTP and WS response channels.
    
    let (human_response_tx, human_response_rx) = crossbeam_channel::unbounded::<crate::api::ApiResponse>();
    
    // Response forwarder thread
    std::thread::spawn(move || {
        while let Ok(resp) = human_response_rx.recv() {
            let _ = response_tx.send(resp.clone());
            let _ = response_tx_ws.send(resp);
        }
    });

    // Create markets
    let markets: Vec<MarketConfig> = config
        .exchange
        .markets
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
        .collect();

    engine.kernel.add_agent(Box::new(ExchangeAgent::new(
        config.exchange.id,
        config.exchange.name.clone(),
        markets,
    )));

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

    // Add regular traders from scenario
    for trader_cfg in &config.traders {
        engine.kernel.add_agent(Box::new(TraderAgent::new(
            trader_cfg.id,
            trader_cfg.name.clone(),
            config.exchange.id,
            trader_cfg.symbol.clone(),
        )));
    }

    // Add smart traders from scenario
    for smart_cfg in &config.smart_traders {
        let side = match smart_cfg.side.to_lowercase().as_str() {
            "short" | "sell" => Side::Sell,
            _ => Side::Buy,
        };

        let strategy = match smart_cfg.strategy.to_lowercase().as_str() {
            "hodler" => TradingStrategy::Hodler {
                side,
                hold_duration_sec: smart_cfg.hold_duration_sec,
                leverage: smart_cfg.leverage,
            },
            "risky" => TradingStrategy::Risky {
                leverage: smart_cfg.leverage,
            },
            "trend_follower" | "trend" => TradingStrategy::TrendFollower {
                lookback_sec: smart_cfg.lookback_sec,
                threshold_pct: smart_cfg.threshold_pct,
                leverage: smart_cfg.leverage,
            },
            _ => TradingStrategy::Risky {
                leverage: smart_cfg.leverage,
            },
        };

        // Support both legacy qty and new qty_min/qty_max
        let qty_min = smart_cfg.qty_min.unwrap_or(smart_cfg.qty);
        let qty_max = smart_cfg.qty_max.unwrap_or(smart_cfg.qty);

        let smart_config = SmartTraderConfig {
            name: smart_cfg.name.clone(),
            exchange_id: config.exchange.id,
            symbol: smart_cfg.symbol.clone(),
            strategy,
            qty_min,
            qty_max,
            wake_interval_ms: smart_cfg.wake_interval_ms,
        };

        engine
            .kernel
            .add_agent(Box::new(SmartTraderAgent::new(smart_cfg.id, smart_config)));
    }

    // Add liquidation agent if configured
    if let Some(liq_cfg) = &config.liquidation_agent {
        let wake_interval_ns = liq_cfg.wake_interval_ms * 1_000_000;
        engine.kernel.add_agent(Box::new(LiquidationAgent::new(
            liq_cfg.id,
            liq_cfg.name.clone(),
            config.exchange.id,
            wake_interval_ns,
        )));
    }

    // Add HumanAgent (id=100, reserved)
    engine.kernel.add_agent(Box::new(HumanAgent::new(
        100,
        "HumanTrader".to_string(),
        config.exchange.id,
        cmd_rx,
        human_response_tx,
        tick_ms,
    )));

    println!();
    println!("=== REALTIME MODE ===");
    println!(
        "Agents: {} traders + {} smart_traders + HumanAgent{}",
        config.traders.len(),
        config.smart_traders.len(),
        if config.liquidation_agent.is_some() { " + LiquidationAgent" } else { "" }
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
