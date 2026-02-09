# Sim-Engine — Trading Simulation on Vara Network

On-chain trading simulation engine for VaraPerps perpetual DEX. Runs bots with various strategies that trade through a real smart contract deployed on Vara Network in real time.

## Architecture

```
┌─────────────────────────────────────────────┐
│  Sim-Engine (Rust)                          │
│                                             │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  │
│  │ Oracle   │  │ Exchange │  │ Keepers  │  │
│  │ (Pyth)   │──│ Agent    │──│          │  │
│  └──────────┘  └────┬─────┘  └──────────┘  │
│                     │                       │
│  ┌──────────┐  ┌────┴─────┐  ┌──────────┐  │
│  │ Market   │  │VaraClient│  │Liquidator│  │
│  │ Maker    │──│(gclient) │──│          │  │
│  └──────────┘  └────┬─────┘  └──────────┘  │
│                     │                       │
│  ┌──────────────────┴──────────────────┐    │
│  │ Smart Traders (Arb, Hodler, Fund..) │    │
│  └─────────────────────────────────────┘    │
│                                             │
│  ┌──────────┐  ┌──────────┐                 │
│  │ HTTP API │  │ WebSocket│  ← Human trader │
│  │ :8080    │  │ :8081    │                  │
│  └──────────┘  └──────────┘                 │
└───────────────┬─────────────────────────────┘
                │ wss://
                ▼
┌───────────────────────────────────────┐
│  Vara Network (Testnet)               │
│  VaraPerps Smart Contract             │
│  Block time: ~3s                      │
└───────────────────────────────────────┘
```

All orders are sent on-chain. Every transaction goes through the real blockchain: SubmitOrder → ExecuteOrder → confirmation in the next block.

## Requirements

- Rust toolchain (stable)
- Deployed VaraPerps contract on Vara testnet
- Keystore with bot keys (gring format)
- Passphrase file for the keystore

## Setup

### Environment Variables

```bash
# Required
export VARA_CONTRACT_ADDRESS="0x..."                   # Deployed VaraPerps contract address
export VARA_KEYSTORE_PATH="/path/to/keystore"          # Path to gring keystore
export VARA_PASSPHRASE_PATH="/path/to/.passphrase"     # Keystore passphrase file

# Optional
export VARA_WS_ENDPOINT="wss://testnet.vara.network"   # RPC endpoint (default: testnet)
export VARA_GAS_LIMIT="200000000000"                   # Base gas limit (default: 100B)
export VARA_BLOCK_TIME_MS="3000"                       # Block time in ms (default: 3000)
export VARA_HUMAN_ADDRESS="kG..."                      # SS58 address for HumanAgent
```

### Gas Limits

Gas automatically scales from `VARA_GAS_LIMIT` (base):

| Operation        | Multiplier | At base=200B |
| ---------------- | :--------: | :----------: |
| Deposit/Withdraw |     1x     |     200B     |
| SubmitOrder      |     1x     |     200B     |
| **ExecuteOrder** |  **1.5x**  |   **300B**   |
| CancelOrder      |    0.5x    |     100B     |

### Keystore

Bot keys are generated via `gring`. Each `agent_id` maps to a keypair through the `AddressBook`:

```
keys/
├── Library/Application Support/gring/   ← keystore
├── .passphrase                          ← passphrase
└── funding_addresses.txt                ← generated automatically
```

## Running

```bash
# Build
cargo build --release

# Run with test_strategies config
cargo run --release -- \
  --scenario test_strategies \
  --realtime \
  --skip-deposits \
  --tick-ms 3000 \
  --port 8080
```

### CLI Arguments

| Argument           | Description                     |   Default    |
| ------------------ | ------------------------------- | :----------: |
| `--scenario NAME`  | Config name (without .json)     | `simple_demo`|
| `--realtime`       | Realtime mode                   |   `false`    |
| `--tick-ms MS`     | Tick interval (= block time)    |    `3000`    |
| `--port PORT`      | HTTP API port                   |    `8080`    |
| `--skip-deposits`  | Skip initial deposits           |   `false`    |

### First Run vs Subsequent Runs

```bash
# First run — deposits to all accounts
cargo run --release -- --scenario test_strategies --realtime --tick-ms 3000 --port 8080

# Subsequent runs — balances already exist on-chain
cargo run --release -- --scenario test_strategies --realtime --skip-deposits --tick-ms 3000 --port 8080
```

## Scenario Configuration

Config files are located at `src/scenarios/*.json`.

### Timing and Block Time

Vara block time ≈ 3 seconds. A transaction takes 2 steps (Submit + Execute) = minimum 2 blocks = **6 seconds**.

Rules for `wake_interval_ms`:
- **Minimum** = `block_time * 2` = 6000ms
- Sending orders faster than the block time is pointless — they will land in the same or next block anyway

Rules for `start_delay_ms`:
- **MarketMaker**: 0 (starts first)
- **Everyone else**: after MM positions are confirmed on-chain (~20s)

### Agent Startup Order

```
t=0s      Oracle + Exchange + MarketMaker
          MM sends SEED orders (Long + Short)

t=6-12s   MM positions confirmed on-chain
          OI sync picks up non-zero positions

t=20-25s  Arbitrageurs + FundingHarvesters + LimitTraders
          See non-zero OI, start trading

t=30-45s  Hodlers + Institutional
          Market is already active
```

### Config Example (Key Fields)

```json
{
  "oracles": [{
    "cache_duration_ms": 3000,
    "wake_interval_ms": 3000
  }],
  "market_maker": {
    "wake_interval_ms": 6000,
    "order_size_tokens": 2.7,
    "leverage": 2
  },
  "keepers": [
    { "wake_interval_ms": 3000 }
  ],
  "smart_traders": [
    {
      "strategy": "arbitrageur",
      "wake_interval_ms": 6000,
      "start_delay_ms": 20000
    },
    {
      "strategy": "hodler",
      "wake_interval_ms": 9000,
      "start_delay_ms": 30000
    }
  ]
}
```

## Bot Strategies

### MarketMaker
Provides liquidity. Monitors OI balance between Long/Short, places SEED orders on the weaker side.

### Arbitrageur
Catches divergence between the on-chain price and oracle (Pyth). Opens a position when deviation > threshold, closes when the price reverts.

### FundingHarvester
Trades the funding rate. Opens a position on the side with positive funding, holds until exit deviation.

### Hodler
Directional bet (Long/Short) with a fixed hold duration. TP/SL based on percentage thresholds.

### Institutional
Large positions with long hold times and moderate leverage.

### Limit Traders
- **MeanReversion** — limit orders at current price ± offset
- **Breakout** — limit orders to catch level breakouts
- **Grid** — grid of orders around the current price
- **Smart** — technical analysis (SMA crossover + RSI + ATR)

### Keepers
Execute pending limit/stop/TP orders when the price reaches the trigger level.

### LiquidationAgent
Scans positions for liquidation, sends liquidation orders.

## API

### HTTP API (`:8080`)

```bash
# Open a position
curl -X POST http://localhost:8080/order -d '{
  "action": "open",
  "symbol": "ETH-USD",
  "side": "long",
  "qty": 1.0,
  "leverage": 5
}'

# Close a position
curl -X POST http://localhost:8080/order -d '{
  "action": "close",
  "symbol": "ETH-USD",
  "side": "long"
}'
```

### WebSocket API (`:8081`)

```javascript
const ws = new WebSocket('ws://localhost:8081');

ws.onopen = () => {
  ws.send(JSON.stringify({
    action: 'open',
    symbol: 'ETH-USD',
    side: 'long',
    qty: 5,
    leverage: 10
  }));
};

ws.onmessage = (event) => {
  const data = JSON.parse(event.data);
  // data.type: 'Event' | 'Response' | 'Error'
  // Event types: OracleTick, OrderExecuted, PositionLiquidated, PositionSnapshot
};
```

## Logs

CSV logs are written to `logs/`:

| File               | Contents                          |
| ------------------ | --------------------------------- |
| `orders.csv`       | All submitted orders              |
| `executions.csv`   | Confirmed executions              |
| `oracle.csv`       | Price ticks                       |
| `positions.csv`    | Position snapshots                |
| `markets.csv`      | OI and liquidity                  |

On-chain transaction results are also logged to `vara_transactions.csv`.

## Project Structure

```
src/
├── main.rs                 # CLI + VaraClient init
├── kernel.rs               # Event loop + message queue
├── sim_engine.rs           # SimEngine wrapper
├── agents/
│   ├── exchange_agent.rs   # Bridge: sim ↔ on-chain contract
│   ├── market_maker_agent.rs
│   ├── smart_trader_agent.rs
│   ├── limit_trader_agent.rs
│   ├── keeper_agent.rs
│   ├── liquidation_agent.rs
│   ├── human_agent.rs      # HTTP/WS → sim messages
│   └── oracle_agent.rs     # Pyth price feed
├── vara/
│   ├── client.rs           # VaraClient (gclient + sails)
│   ├── keystore.rs         # Keypair management
│   ├── types.rs            # Generated types re-export
│   └── vara_perps.idl      # Contract IDL
├── scenarios/
│   ├── simple_demo.rs      # Scenario loader + runner
│   ├── test_strategies.json
│   └── *.json              # Other configs
├── api/
│   ├── server.rs           # HTTP API
│   ├── ws.rs               # WebSocket API
│   ├── pyth.rs             # Pyth price provider
│   └── cache.rs            # Price cache
├── messages.rs             # Message types + SimulatorApi
├── events.rs               # EventBus + CSV logging
├── logging.rs              # CSV loggers
├── latency.rs              # Network latency model
├── pending_orders.rs       # Pending order tracking
└── trigger_checker.rs      # Limit/Stop trigger logic
```

## License

MIT
