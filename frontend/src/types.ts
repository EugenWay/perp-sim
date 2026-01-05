// WebSocket message types from sim-engine

export type Side = 'buy' | 'sell';

export interface OracleTick {
  event_type: 'OracleTick';
  ts: number;
  symbol: string;
  price_min: number;
  price_max: number;
}

export interface PositionSnapshot {
  event_type: 'PositionSnapshot';
  ts: number;
  account: number;
  symbol: string;
  side: Side;
  size_usd: number;
  size_tokens: number;
  collateral: number;
  entry_price: number;
  current_price: number;
  unrealized_pnl: number;
  liquidation_price: number;
  leverage_actual: number;
  is_liquidatable: boolean;
  opened_at_sec: number;
}

export interface MarketSnapshot {
  event_type: 'MarketSnapshot';
  ts: number;
  symbol: string;
  oi_long_usd: number;
  oi_short_usd: number;
  liquidity_usd: number;
}

export interface OrderExecuted {
  event_type: 'OrderExecuted';
  ts: number;
  account: number;
  symbol: string;
  side: Side;
  size_usd: number;
  collateral: number;
  execution_price: number;
  leverage: number;
  order_type: string;
}

export interface OrderLog {
  event_type: 'OrderLog';
  ts: number;
  from: number;
  to: number;
  msg_type: string;
  symbol?: string;
  side?: Side;
  price?: number;
  qty?: number;
}

export type SimEvent = OracleTick | PositionSnapshot | MarketSnapshot | OrderExecuted | OrderLog;

export interface ApiCommand {
  action: 'open' | 'close' | 'status';
  symbol: string;
  side?: string;
  qty?: number;
  leverage?: number;
}

export interface ApiResponse {
  success: boolean;
  message: string;
  data?: unknown;
}

export interface WsMessage {
  type: 'Event' | 'Response' | 'Error';
  payload: SimEvent | ApiResponse | string;
}

// Agent ID to name mapping (from scenario)
export const AGENT_NAMES: Record<number, string> = {
  1: 'Exchange',
  2: 'Oracle',
  10: 'CyclicTrader',
  20: 'HodlerLong',
  21: 'HodlerShort',
  30: 'Risky10x',
  31: 'Risky20x',
  40: 'TrendFollower',
  100: 'Human',
};

// Format micro-USD to readable string
export function formatUsd(microUsd: number): string {
  const usd = microUsd / 1_000_000;
  if (Math.abs(usd) >= 1000000) {
    return `$${(usd / 1000000).toFixed(2)}M`;
  }
  if (Math.abs(usd) >= 1000) {
    return `$${(usd / 1000).toFixed(2)}K`;
  }
  return `$${usd.toFixed(2)}`;
}

// Format price with 4 decimal places
export function formatPrice(microUsd: number): string {
  return `$${(microUsd / 1_000_000).toFixed(4)}`;
}

// Format PnL with color indicator
export function formatPnl(microUsd: number): { text: string; positive: boolean } {
  const usd = microUsd / 1_000_000;
  const sign = usd >= 0 ? '+' : '';
  return {
    text: `${sign}$${usd.toFixed(2)}`,
    positive: usd >= 0,
  };
}

