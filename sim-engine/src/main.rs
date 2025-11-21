pub mod agents;
pub mod api;
mod events;
mod kernel;
mod latency;
mod logging;
mod messages;
pub mod scenarios;
mod sim_engine;

use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    let scenario_name = if args.len() > 1 { &args[1] } else { "simple_demo" };
    
    println!("=== PerpDEX Simulation ===");
    println!("[Main] Running scenario: {}", scenario_name);
    println!();
    
    scenarios::simple_demo::run_scenario(scenario_name);
}
