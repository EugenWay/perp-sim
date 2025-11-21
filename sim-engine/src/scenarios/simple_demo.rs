use crate::agents::{exchange_agent::ExchangeAgent, oracle_agent::OracleAgent, trader_agent::TraderAgent};
use crate::api::{CachedPriceProvider, PythProvider};
use crate::events::{EventListener, SimEvent};
use crate::sim_engine::SimEngine;

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
struct ExchangeConfig {
    id: u32,
    name: String,
    symbol: String,
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
    #[serde(default = "default_trader_wake_interval")]
    wake_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimConfig {
    scenario_name: String,
    duration_sec: u64,
    logs_dir: String,
    exchange: ExchangeConfig,
    oracles: Vec<OracleConfig>,
    traders: Vec<TraderConfig>,
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

    pub fn default() -> Self {
        Self {
            scenario_name: "simple_demo".to_string(),
            duration_sec: 10,
            logs_dir: "logs".to_string(),
            exchange: ExchangeConfig {
                id: 1,
                name: "PerpExchange".to_string(),
                symbol: "PERP-ETH-USD".to_string(),
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
                wake_interval_ms: 2000,
            }],
        }
    }
}

/// Run a simulation with given configuration
fn run_with_config(config: SimConfig) {
    println!("[Scenario] Loading scenario: {}", config.scenario_name);
    println!("[Scenario] Duration: {}s", config.duration_sec);
    println!("[Scenario] Oracles: {}", config.oracles.len());
    println!("[Scenario] Traders: {}", config.traders.len());

    let max_ticks = (config.duration_sec * 1000 / 100) as usize;

    let mut engine = SimEngine::with_default_latency();

    {
        let _ = fs::create_dir_all(&config.logs_dir);
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

    engine.kernel.add_agent(Box::new(ExchangeAgent::new(
        config.exchange.id,
        config.exchange.name.clone(),
        config.exchange.symbol.clone(),
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
            config.exchange.symbol.clone(),
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
