// src/scenarios/simple_demo.rs
// Simple scenario: one exchange + oracle + trader.
// Logging now goes through EventBus + CSV, without a separate LoggerAgent.

use crate::agents::{exchange_agent::ExchangeAgent, oracle_agent::OracleAgent, trader_agent::TraderAgent};
use crate::events::{EventListener, SimEvent};
use crate::sim_engine::SimEngine;

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{BufWriter, Write};

/// Helper struct to wrap a closure as an EventListener
struct ClosureListener<F: FnMut(&SimEvent)> {
    closure: F,
}

impl<F: FnMut(&SimEvent)> EventListener for ClosureListener<F> {
    fn on_event(&mut self, event: &SimEvent) {
        (self.closure)(event);
    }
}

/// Run a small demo simulation.
pub fn run() {
    let symbol = "PERP-ETH-USD".to_string();

    let exchange_id: u32 = 1;
    let oracle_id: u32 = 2;
    let trader_id: u32 = 3;

    let mut engine = SimEngine::with_default_latency();

    //
    // CSV logger for exchange orders — analog of TS CsvLog + kernel.on(ORDER_LOG)
    //
    {
        // Create logs directory, ignore error if it already exists.
        let _ = fs::create_dir_all("logs");

        let file = File::create("logs/orders.csv").expect("cannot create logs/orders.csv");
        let writer = RefCell::new(BufWriter::new(file));

        // Simple header, can be extended later.
        writeln!(writer.borrow_mut(), "ts,from,to,msg_type,symbol,side,price,qty,reason")
            .expect("cannot write CSV header");

        // Subscribe to OrderLog events.
        // Writer moves into closure and lives until end of simulation.
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
        // Writer stays alive inside listener, so no separate variable needed.
    }

    // --- Agent registration ---

    engine.kernel.add_agent(Box::new(ExchangeAgent::new(
        exchange_id,
        "PerpExchange".to_string(),
        symbol.clone(),
    )));

    engine.kernel.add_agent(Box::new(OracleAgent::new(
        oracle_id,
        "Oracle".to_string(),
        symbol.clone(),
        exchange_id,
    )));

    engine.kernel.add_agent(Box::new(TraderAgent::new(
        trader_id,
        "Trader1".to_string(),
        exchange_id,
        symbol.clone(),
    )));

    // Oracle and Trader schedule their own wakeups in on_start.
    println!("[Scenario] starting simple_demo");
    engine.run(50);
    println!("[Scenario] finished simple_demo");
}
