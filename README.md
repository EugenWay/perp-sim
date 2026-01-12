# PerpDEX Simulator

Event-driven perpetual DEX trading simulator with real-time Pyth Network price feeds and integrated `perp-futures` exchange engine.

## Features

- **Real-time price feeds** from Pyth Network
- **Multiple trading agents**: Cyclic, Hodler, Risky, TrendFollower
- **Human trading** via HTTP API in realtime mode
- **Comprehensive logging** to CSV files
- **Configurable scenarios** via JSON

## Build

```bash
# Build release version
cargo build --release

# Binary location
./target/release/sim-engine
```

## Usage

### Fast-forward Mode (default)

Runs simulation as fast as possible for the configured duration.

```bash
# Run default scenario (simple_demo, 180 seconds)
./target/release/sim-engine

# Run specific scenario
./target/release/sim-engine -s multi_agent
./target/release/sim-engine --scenario my_scenario
```

### Realtime Mode

Runs simulation in real-time with HTTP API for manual trading.

```bash
# Default: 100ms tick, port 8080
./target/release/sim-engine --realtime

# Custom tick interval (1 second) and port
./target/release/sim-engine --realtime --tick-ms 1000 --port 9000

# With specific scenario
./target/release/sim-engine -s simple_demo --realtime -t 1000 -p 8080
```

### CLI Options

| Option       | Short | Default       | Description                   |
| ------------ | ----- | ------------- | ----------------------------- |
| `--scenario` | `-s`  | `simple_demo` | Scenario name (without .json) |
| `--realtime` | `-r`  | `false`       | Enable realtime mode          |
| `--tick-ms`  | `-t`  | `100`         | Tick interval in milliseconds |
| `--port`     | `-p`  | `8080`        | HTTP API port (realtime only) |

## HTTP API (Realtime Mode)

### Open Position

```bash
curl -X POST http://localhost:8080/order \
  -H "Content-Type: application/json" \
  -d '{
    "action": "open",
    "symbol": "ETH-USD",
    "side": "long",
    "qty": 1,
    "leverage": 5
  }'
```

**Response:**

```json
{
  "success": true,
  "message": "Order: ETH-USD Buy qty=1 lev=5x",
  "data": { "symbol": "ETH-USD", "side": "Buy", "qty": 1, "leverage": 5 }
}
```

### Close Position

```bash
curl -X POST http://localhost:8080/close \
  -H "Content-Type: application/json" \
  -d '{"symbol": "ETH-USD"}'
```

### Get Status

```bash
curl http://localhost:8080/status
```

### Health Check

```bash
curl http://localhost:8080/health
```

## Scenario Configuration

Scenarios are JSON files in `sim-engine/src/scenarios/`.

### Example: `simple_demo.json`

```json
{
  "scenario_name": "simple_demo",
  "duration_sec": 180,
  "logs_dir": "logs",
  "exchange": {
    "id": 1,
    "name": "PerpExchange",
    "markets": [
      {
        "id": 0,
        "symbol": "ETH-USD",
        "index_token": "ETH",
        "collateral_token": "USDT",
        "initial_liquidity": {
          "collateral_amount": 10000000000000,
          "index_amount": 5000000000000,
          "liquidity_usd": 20000000000000
        }
      }
    ]
  },
  "oracles": [
    {
      "id": 2,
      "name": "PythOracle",
      "symbols": ["ETH-USD"],
      "provider": "Pyth",
      "cache_duration_ms": 5000,
      "wake_interval_ms": 5000
    }
  ],
  "traders": [
    {
      "id": 10,
      "name": "CyclicTrader",
      "symbol": "ETH-USD",
      "wake_interval_ms": 10000
    }
  ],
  "smart_traders": [
    {
      "id": 20,
      "name": "HodlerLong",
      "symbol": "ETH-USD",
      "strategy": "hodler",
      "side": "long",
      "leverage": 5,
      "qty": 1,
      "hold_duration_sec": 120
    }
  ]
}
```

## Units of Measurement

### External (API, Logs, Frontend)

| Value               | Unit        | Scale    | Example                  |
| ------------------- | ----------- | -------- | ------------------------ |
| **Prices**          | micro-USD   | 1e-6 USD | `2939000000` = $2,939.00 |
| **Size/Collateral** | micro-USD   | 1e-6 USD | `585000000` = $585.00    |
| **Liquidity**       | micro-USD   | 1e-6 USD | `20000000000000` = $20M  |
| **Timestamps**      | nanoseconds | 1e-9 sec | Unix epoch in ns         |
| **Leverage**        | integer     | 1x       | `5` = 5x leverage        |

### Internal (perp-futures engine)

| Value               | Unit            | Scale      | Example                          |
| ------------------- | --------------- | ---------- | -------------------------------- |
| **Prices**          | USD per atom    | 1e30       | ETH: `3000 * 10^12` per wei      |
| **Size/OI**         | USD             | 1e30       | `585 * 10^30` = $585             |
| **Collateral**      | atoms           | 10^decimals| USDC: `585000000` (6 decimals)   |
| **Liquidity**       | USD             | 1e30       | `20_000_000 * 10^30` = $20M      |

### Price Normalization

Conversion between external micro-USD and internal USD(1e30) per atom:

```rust
// External → Internal (at SimOracle boundary)
price_per_atom = price_micro_usd * 10^(24 - token_decimals)

// Examples:
// ETH ($3000, 18 decimals):  3_000_000_000 * 10^6  = 3000 * 10^12 per wei
// BTC ($100k, 8 decimals):   100_000_000_000 * 10^16 = 100000 * 10^22 per satoshi
// USDC ($1, 6 decimals):     1_000_000 * 10^18 = 10^24 per atom

// Internal → External (for display)
price_micro_usd = price_per_atom / 10^(24 - token_decimals)
```

### Converting Values

```python
# External: micro-USD to USD
usd = micro_usd / 1_000_000

# Example: 2939000000 -> $2,939.00
price = 2939000000 / 1_000_000  # = 2939.0

# Internal: USD(1e30) to USD
usd = usd_1e30 / 1e30

# Example: 585 * 10^30 -> $585.00
size = (585 * 10**30) / 10**30  # = 585.0
```

## Output Logs

All logs are saved to `logs/` directory:

| File             | Description                                      |
| ---------------- | ------------------------------------------------ |
| `oracle.csv`     | Price updates (timestamp, symbol, min, max, mid) |
| `orders.csv`     | Order submissions (before execution)             |
| `executions.csv` | Executed orders with prices                      |
| `positions.csv`  | Position snapshots with PnL                      |
| `markets.csv`    | Market state (OI, liquidity)                     |

### Example: `positions.csv`

```csv
ts,account,symbol,side,size_usd,size_tokens,collateral,entry_price,current_price,pnl,liquidation_price,leverage,is_liquidatable,opened_at
1766918964297046000,10,ETH-USD,Buy,2925820000,1,585164000,2925820000,2939000000,13180000,2340656000,5,false,1766918964
```

## Trading Agents

### Built-in Strategies

| Agent             | Strategy      | Description                         |
| ----------------- | ------------- | ----------------------------------- |
| **CyclicTrader**  | Alternating   | Opens long/short alternately        |
| **Hodler**        | Hold          | Opens position, holds for N seconds |
| **Risky**         | High leverage | Random side, high leverage (10-20x) |
| **TrendFollower** | Momentum      | Follows price trend with lookback   |
| **HumanAgent**    | Manual        | Controlled via HTTP API             |

## Supported Symbols

- ETH-USD
- BTC-USD
- SOL-USD
- AVAX-USD
- MATIC-USD

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                      Kernel                              │
│  (Event loop, message queue, virtual time)              │
└─────────────────────────────────────────────────────────┘
         │              │              │
         ▼              ▼              ▼
┌─────────────┐ ┌─────────────┐ ┌─────────────┐
│ OracleAgent │ │ExchangeAgent│ │ TraderAgent │
│ (Pyth API)  │ │(perp-futures│ │  (Bots)     │
└─────────────┘ │  Executor)  │ └─────────────┘
                └─────────────┘
                       │
                       ▼
              ┌─────────────────┐
              │   CSV Loggers   │
              └─────────────────┘
```

## Engine Integration

The simulator uses `perp-futures` engine API for:

- ✅ **Liquidation checks**: `executor.is_liquidatable_by_margin()`
- ✅ **Liquidation price**: `executor.calculate_liquidation_price()`
- ✅ **Order execution**: All orders executed through `executor.execute_order()`
- ✅ **PnL calculation**: Engine handles PnL with fees, funding, borrowing, price impact

See `PERP_FUTURES_ISSUES.md` for known engine limitations.

## License

MIT
