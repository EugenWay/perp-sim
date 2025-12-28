pub mod agents;
pub mod api;
mod events;
mod kernel;
mod latency;
mod logging;
mod messages;
pub mod scenarios;
mod sim_engine;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "sim-engine")]
#[command(about = "Perpetual DEX trading simulation")]
struct Args {
    /// Scenario name (without .json extension)
    #[arg(short, long, default_value = "simple_demo")]
    scenario: String,

    /// Enable realtime mode
    #[arg(short, long, default_value = "false")]
    realtime: bool,

    /// Realtime tick interval in milliseconds
    #[arg(short = 't', long, default_value = "100")]
    tick_ms: u64,

    /// HTTP API port for HumanAgent (only in realtime mode)
    #[arg(short, long, default_value = "8080")]
    port: u16,
}

fn main() {
    let args = Args::parse();

    println!("=== PerpDEX Simulation ===");
    println!("[Main] Scenario: {}", args.scenario);
    if args.realtime {
        println!("[Main] Mode: REALTIME ({}ms tick)", args.tick_ms);
        println!("[Main] API port: {}", args.port);
    } else {
        println!("[Main] Mode: Fast-forward");
    }
    println!();

    if args.realtime {
        scenarios::simple_demo::run_realtime(&args.scenario, args.tick_ms, args.port);
    } else {
        scenarios::simple_demo::run_scenario(&args.scenario);
    }
}
