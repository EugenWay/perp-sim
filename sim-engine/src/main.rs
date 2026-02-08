pub mod agents;
pub mod api;
mod events;
mod kernel;
mod latency;
mod logging;
mod messages;
mod pending_orders;
pub mod scenarios;
mod sim_engine;
mod trigger_checker;
pub mod vara;

use clap::Parser;
use std::sync::Arc;

use vara::{VaraClient, VaraConfig};

#[derive(Parser, Debug)]
#[command(name = "sim-engine")]
#[command(about = "Perpetual DEX trading simulation on Vara Network")]
struct Args {
    /// Scenario name (without .json extension)
    #[arg(short, long, default_value = "simple_demo")]
    scenario: String,

    /// Enable realtime mode
    #[arg(short, long, default_value = "false")]
    realtime: bool,

    /// Realtime tick interval in milliseconds (should match block time ~3000ms)
    #[arg(short = 't', long, default_value = "3000")]
    tick_ms: u64,

    /// HTTP API port for HumanAgent (only in realtime mode)
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Skip initial deposits (use when balances already exist on-chain)
    #[arg(long, default_value = "false")]
    skip_deposits: bool,
}

fn main() {
    let args = Args::parse();

    println!("=== PerpDEX on Vara Network ===");
    println!("[Main] Scenario: {}", args.scenario);
    if args.realtime {
        println!("[Main] Mode: REALTIME ({}ms tick)", args.tick_ms);
        println!("[Main] API port: {}", args.port);
    } else {
        println!("[Main] Mode: Fast-forward");
    }

    // Initialize Vara connection (required)
    let vara_client = match init_vara_client() {
        Ok(client) => {
            println!("[Vara] Ready for blockchain operations");
            Arc::new(client)
        }
        Err(e) => {
            eprintln!("[Vara] FATAL: Failed to initialize: {}", e);
            eprintln!();
            eprintln!("Required environment variables:");
            eprintln!("  VARA_CONTRACT_ADDRESS - deployed VaraPerps contract address");
            eprintln!("  VARA_WS_ENDPOINT      - WebSocket RPC (default: wss://testnet.vara.network)");
            eprintln!("  VARA_KEYSTORE_PATH    - path to gring keystore (default: keys/Library/...)");
            eprintln!("  VARA_PASSPHRASE_PATH  - path to passphrase file (default: keys/.passphrase)");
            eprintln!("  VARA_PASSPHRASE_FILE  - legacy name for passphrase path");
            std::process::exit(1);
        }
    };

    println!();

    if args.realtime {
        scenarios::simple_demo::run_realtime_with_blockchain(
            &args.scenario,
            args.tick_ms,
            args.port,
            args.skip_deposits,
            vara_client,
        );
    } else {
        scenarios::simple_demo::run_scenario_with_blockchain(
            &args.scenario,
            args.skip_deposits,
            vara_client,
        );
    }
}

/// Initialize VaraClient for blockchain operations
fn init_vara_client() -> Result<VaraClient, vara::VaraError> {
    let config = VaraConfig::from_env()?;

    println!("[Vara] Endpoint: {}", config.ws_endpoint);
    println!("[Vara] Contract: {}", config.contract_address);
    println!("[Vara] Keystore: {}", config.keystore_path);

    let mut client = VaraClient::new(config)?;
    client.connect()?;

    // Preload bot keypairs
    match client.preload_keypairs(200) {
        Ok(count) => println!("[Vara] Loaded {} bot keypairs", count),
        Err(e) => eprintln!("[Vara] Warning: Could not preload keypairs: {}", e),
    }

    Ok(client)
}
