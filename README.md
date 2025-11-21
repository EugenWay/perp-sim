# PerpDEX Simulator

Event-driven perpetual DEX simulator with Pyth Network price feeds.

## Quick Start

```bash
# Build
cargo build --release

# Run default scenario (simple_demo)
cargo run --bin sim-engine

# Run specific scenario
≈
cargo run --bin sim-engine multi_agent
```

## Configuration

Scenarios in `sim-engine/src/scenarios/*.json`:

```json
{
  "scenario_name": "simple_demo",
  "duration_sec": 10,
  "oracles": [
    {
      "symbols": ["ETH-USD", "USDT-USD"],
      "cache_duration_ms": 10000,
      "wake_interval_ms": 3000
    }
  ],
  "traders": [
    {
      "wake_interval_ms": 2000
    }
  ]
}
```

**Parameters:**

- `duration_sec` - simulation duration (seconds)
- `wake_interval_ms` - agent update frequency (milliseconds)
- `cache_duration_ms` - price cache duration (milliseconds)

**Supported symbols:** BTC-USD, ETH-USD, SOL-USD, AVAX-USD, MATIC-USD, USDT-USD

## Output

Logs saved to `logs/`:

- `orders.csv` - trading activity
- `oracle.csv` - price updates

## Pyth Network Integration

See `PYTH_API.md` for API details.
