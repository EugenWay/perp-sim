// src/main.rs
// Program entrypoint: selects a scenario and runs it.

pub mod agents;
mod events;
mod kernel;
mod latency;
mod logging;
mod messages;
pub mod scenarios;
mod sim_engine;

fn main() {
    println!("=== PerpDEX simulation: simple_demo ===");
    scenarios::simple_demo::run();
}
