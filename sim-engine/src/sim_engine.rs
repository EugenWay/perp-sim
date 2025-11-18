// src/sim_engine.rs
// High-level simulation engine wrapper around the Kernel.
// Scenarios can create a SimEngine, register agents, and run it.

use std::path::Path;

use crate::events::EventListener;
use crate::kernel::Kernel;
use crate::latency::{FixedLatency, LatencyModel};
use crate::logging::{CsvOracleLogger, CsvOrderLogger};

/// High-level simulation engine.
/// Right now it is just a thin wrapper around Kernel, but later you can
/// add logging, metrics, configuration, etc.
pub struct SimEngine {
    pub kernel: Kernel,
}

impl SimEngine {
    /// Create a new engine with a custom latency model and tick size.
    /// Optionally attach CSV loggers via EventBus.
    pub fn new(latency: Box<dyn LatencyModel>, tick_ns: u64, logs_dir: Option<&Path>) -> Self {
        let mut kernel = Kernel::new(latency, tick_ns);

        if let Some(dir) = logs_dir {
            // Try to create CSV loggers, don't panic on error, just log.
            match CsvOrderLogger::new(dir) {
                Ok(logger) => {
                    kernel
                        .event_bus_mut()
                        .subscribe(Box::new(logger) as Box<dyn EventListener>);
                    println!("[SimEngine] CsvOrderLogger attached");
                }
                Err(e) => eprintln!("[SimEngine] failed to init CsvOrderLogger: {e}"),
            }

            match CsvOracleLogger::new(dir) {
                Ok(logger) => {
                    kernel
                        .event_bus_mut()
                        .subscribe(Box::new(logger) as Box<dyn EventListener>);
                    println!("[SimEngine] CsvOracleLogger attached");
                }
                Err(e) => eprintln!("[SimEngine] failed to init CsvOracleLogger: {e}"),
            }
        }

        Self { kernel }
    }

    /// Create an engine with a default latency model and CSV logging into ./logs.
    pub fn with_default_latency() -> Self {
        // Example: 1ms network delay + 0.5ms compute delay, 100ms simulation tick.
        let latency: Box<dyn LatencyModel> = Box::new(FixedLatency::new(1_000_000, 500_000));
        let tick_ns = 100_000_000;
        Self::new(latency, tick_ns, Some(Path::new("logs")))
    }

    /// Run the underlying kernel for a number of ticks.
    pub fn run(&mut self, max_steps: usize) {
        self.kernel.run(max_steps);
    }
}
