# Pyth Network API

## Endpoint
```
https://hermes.pyth.network/v2/updates/price/latest
```

## Request Example

```bash
curl "https://hermes.pyth.network/v2/updates/price/latest?ids[]=0xff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace"
```

## Price Feed IDs

| Symbol | Feed ID |
|--------|---------|
| BTC-USD | `0xe62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43` |
| ETH-USD | `0xff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace` |
| SOL-USD | `0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d` |
| USDT-USD | `0x2b89b9dc8fdf9f34709a5b106b472f0f39bb6ca9ce04b0fd7f2e971688e2e53b` |

## Response Structure

```json
{
  "binary": {
    "encoding": "base64",
    "data": [
      "UE5BVQEAAAADuAEAAAADDQ...3318 bytes (VAA signature)"
    ]
  },
  "parsed": [
    {
      "id": "0xff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace",
      "price": {
        "price": "268347000000",
        "conf": "174000000",
        "expo": -8,
        "publish_time": 1732201234
      },
      "ema_price": {
        "price": "268350000000",
        "conf": "200000000",
        "expo": -8,
        "publish_time": 1732201234
      }
    }
  ]
}
```

## Key Fields

**binary.data** - VAA signature (base64, ~3318 bytes)
- Wormhole guardian signatures
- Cryptographic proof of price data
- Used for on-chain verification

**parsed[].price**
- `price` - Price value (apply expo: price * 10^expo)
- `conf` - Confidence interval
- `expo` - Exponent (usually -8)
- `publish_time` - Unix timestamp

**Example:** 
```
price = 268347000000
expo = -8
actual_price = 268347000000 * 10^(-8) = 2683.47 USD
```

## Batch Request

```bash
curl "https://hermes.pyth.network/v2/updates/price/latest?ids[]=0xff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace&ids[]=0xe62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43"
```

Returns multiple price feeds in `parsed[]` array.

