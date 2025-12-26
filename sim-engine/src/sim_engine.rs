use std::path::Path;

use crate::events::EventListener;
use crate::kernel::Kernel;
use crate::latency::{FixedLatency, LatencyModel};
use crate::logging::{CsvExecutionLogger, CsvOracleLogger, CsvOrderLogger};

pub struct SimEngine {
    pub kernel: Kernel,
}

impl SimEngine {
    pub fn new(latency: Box<dyn LatencyModel>, tick_ns: u64, logs_dir: Option<&Path>) -> Self {
        let mut kernel = Kernel::new(latency, tick_ns);

        if let Some(dir) = logs_dir {
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

            match CsvExecutionLogger::new(dir) {
                Ok(logger) => {
                    kernel
                        .event_bus_mut()
                        .subscribe(Box::new(logger) as Box<dyn EventListener>);
                    println!("[SimEngine] CsvExecutionLogger attached");
                }
                Err(e) => eprintln!("[SimEngine] failed to init CsvExecutionLogger: {e}"),
            }
        }

        Self { kernel }
    }

    pub fn with_default_latency() -> Self {
        let latency: Box<dyn LatencyModel> = Box::new(FixedLatency::new(1_000_000, 500_000));
        let tick_ns = 100_000_000; // 100ms tick
        Self::new(latency, tick_ns, Some(Path::new("logs")))
    }

    pub fn run(&mut self, max_steps: usize) {
        self.kernel.run(max_steps);
    }
}
