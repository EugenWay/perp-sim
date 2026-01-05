import { useEffect, useState, useRef } from 'react';
import './App.css';
import {
  SimEvent,
  WsMessage,
  ApiCommand,
  OracleTick,
  PositionSnapshot,
  MarketSnapshot,
  OrderExecuted,
  AGENT_NAMES,
  formatUsd,
  formatPrice,
  formatPnl,
  Side,
} from './types';

const WS_URL = 'ws://localhost:8081';
const HUMAN_AGENT_ID = 100;
const INITIAL_BALANCE = 10_000_000_000; // $10,000 in micro-USD
const STORAGE_KEY = 'perp-lab-human-positions';

interface LogEntry {
  id: number;
  ts: number;
  text: string;
  type: 'trader' | 'exchange' | 'oracle' | 'error' | 'success';
}

// LocalStorage helpers
function savePositionsToStorage(positions: PositionSnapshot[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(positions));
  } catch (e) {
    console.warn('Failed to save positions to localStorage:', e);
  }
}

function loadPositionsFromStorage(): PositionSnapshot[] {
  try {
    const data = localStorage.getItem(STORAGE_KEY);
    if (data) {
      return JSON.parse(data);
    }
  } catch (e) {
    console.warn('Failed to load positions from localStorage:', e);
  }
  return [];
}

function App() {
  const [connected, setConnected] = useState(false);
  const [prices, setPrices] = useState<Record<string, { min: number; max: number }>>({});
  const [markets, setMarkets] = useState<Record<string, MarketSnapshot>>({});
  const [positions, setPositions] = useState<Record<string, PositionSnapshot>>({});
  const [humanPositions, setHumanPositions] = useState<PositionSnapshot[]>([]);
  const [humanBalance, setHumanBalance] = useState(INITIAL_BALANCE);
  const [traderLogs, setTraderLogs] = useState<LogEntry[]>([]);
  const [exchangeLogs, setExchangeLogs] = useState<LogEntry[]>([]);

  // Trading form state
  const [selectedSymbol, setSelectedSymbol] = useState('ETH-USD');
  const [marginPercent, setMarginPercent] = useState(10); // 0-100% of available balance
  const [leverage, setLeverage] = useState(5);

  const wsRef = useRef<WebSocket | null>(null);
  const logIdRef = useRef(0);
  const seenEventIds = useRef<Set<string>>(new Set());

  const addTraderLog = (text: string, type: LogEntry['type'] = 'trader') => {
    const entry: LogEntry = { id: logIdRef.current++, ts: Date.now(), text, type };
    setTraderLogs(prev => [entry, ...prev].slice(0, 100));
  };

  const addExchangeLog = (text: string, type: LogEntry['type'] = 'exchange') => {
    const entry: LogEntry = { id: logIdRef.current++, ts: Date.now(), text, type };
    setExchangeLogs(prev => [entry, ...prev].slice(0, 100));
  };

  const handleEvent = (event: SimEvent) => {
    // Deduplicate events using timestamp + type + account
    const eventId = `${event.ts}-${event.event_type}-${'account' in event ? event.account : ''}`;
    if (seenEventIds.current.has(eventId)) {
      return; // Skip duplicate
    }
    seenEventIds.current.add(eventId);
    // Keep only last 1000 event IDs to prevent memory leak
    if (seenEventIds.current.size > 1000) {
      const arr = Array.from(seenEventIds.current);
      seenEventIds.current = new Set(arr.slice(-500));
    }

    switch (event.event_type) {
      case 'OracleTick': {
        const e = event as OracleTick;
        setPrices(prev => ({
          ...prev,
          [e.symbol]: { min: e.price_min, max: e.price_max },
        }));
        // Don't log every oracle tick - too noisy
        break;
      }

      case 'MarketSnapshot': {
        const e = event as MarketSnapshot;
        setMarkets(prev => ({ ...prev, [e.symbol]: e }));
        break;
      }

      case 'PositionSnapshot': {
        const e = event as PositionSnapshot;
        const key = `${e.account}-${e.symbol}-${e.side}`;
        setPositions(prev => ({ ...prev, [key]: e }));

        if (e.account === HUMAN_AGENT_ID) {
          setHumanPositions(prev => {
            const filtered = prev.filter(
              p => !(p.symbol === e.symbol && p.side === e.side)
            );
            const updated = [...filtered, e];
            // Calculate total collateral from ALL human positions
            const totalCollateral = updated.reduce((sum, p) => sum + p.collateral, 0);
            setHumanBalance(INITIAL_BALANCE - totalCollateral);
            return updated;
          });
        }
        break;
      }

      case 'OrderExecuted': {
        const e = event as OrderExecuted;
        const agentName = AGENT_NAMES[e.account] || `Agent#${e.account}`;
        const side = e.side === 'buy' ? 'LONG' : 'SHORT';
        const action = e.order_type === 'Increase' ? '📈 OPEN' : '📉 CLOSE';
        const priceStr = formatPrice(e.execution_price);
        const sizeStr = formatUsd(e.size_usd);

        if (e.account === HUMAN_AGENT_ID) {
          addTraderLog(
            `${action} ${side} ${sizeStr} @ ${priceStr}`,
            'success'
          );
          // Update balance on close
          if (e.order_type === 'Decrease') {
            setHumanBalance(prev => prev + e.collateral);
          }
        } else {
          addTraderLog(
            `${agentName}: ${action} ${side} ${sizeStr} @ ${priceStr} (${e.leverage}x)`,
            'trader'
          );
        }

        addExchangeLog(
          `✅ ${agentName} ${side} ${sizeStr}`,
          'exchange'
        );
        break;
      }

      default:
        break;
    }
  };

  // Load positions from localStorage on mount
  useEffect(() => {
    const savedPositions = loadPositionsFromStorage();
    if (savedPositions.length > 0) {
      setHumanPositions(savedPositions);
      // Calculate balance from saved positions
      const totalCollateral = savedPositions.reduce((sum, p) => sum + p.collateral, 0);
      setHumanBalance(INITIAL_BALANCE - totalCollateral);
      console.log(`[Storage] Loaded ${savedPositions.length} positions from localStorage`);
    }
  }, []);

  // Save positions to localStorage when they change
  useEffect(() => {
    savePositionsToStorage(humanPositions);
  }, [humanPositions]);

  // Request balance from API
  const requestBalance = (ws: WebSocket) => {
    if (ws.readyState === WebSocket.OPEN) {
      const cmd: ApiCommand = { action: 'balance', symbol: 'ETH-USD' };
      ws.send(JSON.stringify(cmd));
    }
  };

  // WebSocket connection - only run once on mount
  useEffect(() => {
    let reconnectTimeout: ReturnType<typeof setTimeout>;
    let isMounted = true;

    const connect = () => {
      if (!isMounted) return;
      
      const ws = new WebSocket(WS_URL);

      ws.onopen = () => {
        if (!isMounted) return;
        setConnected(true);
        addExchangeLog('🟢 Connected to simulator', 'success');
        // Request balance after connection
        requestBalance(ws);
      };

      ws.onclose = () => {
        if (!isMounted) return;
        setConnected(false);
        addExchangeLog('🔴 Disconnected', 'error');
        // Reconnect after 2 seconds
        reconnectTimeout = setTimeout(connect, 2000);
      };

      ws.onerror = () => {
        // Error logged on close
      };

      ws.onmessage = (msg) => {
        if (!isMounted) return;
        try {
          const data: WsMessage = JSON.parse(msg.data);
          if (data.type === 'Event') {
            handleEvent(data.payload as SimEvent);
          } else if (data.type === 'Response') {
            const resp = data.payload as { 
              success: boolean; 
              message: string; 
              data?: { 
                initial_balance?: number;
                available_balance?: number;
                collateral_used?: number;
              };
            };
            // Handle balance response
            if (resp.data?.initial_balance !== undefined) {
              setHumanBalance(resp.data.available_balance || INITIAL_BALANCE);
              console.log('[API] Balance:', resp.data);
            } else {
              addTraderLog(
                resp.message,
                resp.success ? 'success' : 'error'
              );
            }
          } else if (data.type === 'Error') {
            addTraderLog(`Error: ${data.payload}`, 'error');
          }
        } catch (e) {
          console.error('Parse error:', e);
        }
      };

      wsRef.current = ws;
    };

    connect();

    return () => {
      isMounted = false;
      clearTimeout(reconnectTimeout);
      wsRef.current?.close();
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []); // Empty deps - connect only once

  const sendCommand = (cmd: ApiCommand) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(cmd));
    } else {
      addTraderLog('Not connected to simulator', 'error');
    }
  };

  const openPosition = (side: 'long' | 'short') => {
    if (midPrice <= 0 || estimatedQty <= 0) {
      addTraderLog('Cannot trade: invalid price or quantity', 'error');
      return;
    }
    if (estimatedCollateral > humanBalance) {
      addTraderLog('Insufficient balance for this trade', 'error');
      return;
    }
    // Round to nearest whole token, minimum 1
    const qtyToSend = Math.max(1, Math.round(estimatedQty));
    sendCommand({
      action: 'open',
      symbol: selectedSymbol,
      side,
      qty: qtyToSend,
      leverage,
    });
    addTraderLog(`Sending ${side.toUpperCase()} order for ${qtyToSend} tokens...`, 'trader');
  };

  const closePosition = (symbol: string, side: Side) => {
    sendCommand({
      action: 'close',
      symbol,
      side: side === 'buy' ? 'long' : 'short',
    });
    addTraderLog(`Closing ${symbol} position...`, 'trader');
  };

  // Get available symbols from prices
  const symbols = Object.keys(prices);
  if (symbols.length > 0 && !symbols.includes(selectedSymbol)) {
    setSelectedSymbol(symbols[0]);
  }

  // Get current price for selected symbol
  const currentPrice = prices[selectedSymbol];
  const midPrice = currentPrice ? (currentPrice.min + currentPrice.max) / 2 : 0;

  // Calculate based on margin percentage
  // Collateral = humanBalance * (marginPercent / 100)
  // Size = Collateral * leverage
  // Qty (tokens) = Size / price
  const estimatedCollateral = humanBalance * (marginPercent / 100);
  const estimatedSize = estimatedCollateral * leverage;
  const estimatedQty = midPrice > 0 ? estimatedSize / midPrice : 0;
  const maxMarginForBalance = humanBalance; // Max collateral we can use
  const canTrade = connected && midPrice > 0 && estimatedCollateral > 0 && estimatedCollateral <= humanBalance;

  return (
    <div className="app">
      {/* Header with prices */}
      <header className="header">
        <div className="prices">
          {Object.entries(prices).map(([symbol, price]) => (
            <div key={symbol} className="price-item">
              <span className="symbol">{symbol}</span>
              <span className="price">{formatPrice((price.min + price.max) / 2)}</span>
            </div>
          ))}
          {symbols.length === 0 && (
            <div className="price-item">
              <span className="symbol">Waiting for prices...</span>
            </div>
          )}
        </div>
        <div className={`connection ${connected ? 'connected' : 'disconnected'}`}>
          {connected ? '● Connected' : '○ Disconnected'}
        </div>
      </header>

      {/* Main content - 3 columns */}
      <main className="main">
        {/* Column 1: Traders Log */}
        <section className="column traders-column">
          <h2>📊 Traders Activity</h2>
          <div className="log-container">
            {traderLogs.map(log => (
              <div key={log.id} className={`log-entry ${log.type}`}>
                <span className="log-time">
                  {new Date(log.ts).toLocaleTimeString()}
                </span>
                <span className="log-text">{log.text}</span>
              </div>
            ))}
            {traderLogs.length === 0 && (
              <div className="log-entry empty">Waiting for activity...</div>
            )}
          </div>
        </section>

        {/* Column 2: Exchange Log */}
        <section className="column exchange-column">
          <h2>🏛️ Exchange & Oracle</h2>

          {/* Market Stats */}
          {Object.entries(markets).map(([symbol, market]) => (
            <div key={symbol} className="market-stats">
              <h3>{symbol}</h3>
              <div className="stat-row">
                <span>OI Long:</span>
                <span className="long">{formatUsd(market.oi_long_usd)}</span>
              </div>
              <div className="stat-row">
                <span>OI Short:</span>
                <span className="short">{formatUsd(market.oi_short_usd)}</span>
              </div>
              <div className="stat-row">
                <span>Liquidity:</span>
                <span>{formatUsd(market.liquidity_usd)}</span>
              </div>
            </div>
          ))}

          <div className="log-container">
            {exchangeLogs.map(log => (
              <div key={log.id} className={`log-entry ${log.type}`}>
                <span className="log-time">
                  {new Date(log.ts).toLocaleTimeString()}
                </span>
                <span className="log-text">{log.text}</span>
              </div>
            ))}
          </div>
        </section>

        {/* Column 3: Human Trading Interface */}
        <section className="column human-column">
          <h2>🧑‍💻 Human Trader</h2>
          
          {/* Balance */}
          <div className="balance-display">
            <span className="balance-label">Balance:</span>
            <span className="balance-value">{formatUsd(humanBalance)}</span>
          </div>

          {/* Trading Form */}
          <div className="trading-form">
            <div className="form-group">
              <label>Symbol</label>
              <select
                value={selectedSymbol}
                onChange={e => setSelectedSymbol(e.target.value)}
              >
                {symbols.map(s => (
                  <option key={s} value={s}>{s}</option>
                ))}
                {symbols.length === 0 && (
                  <option value="ETH-USD">ETH-USD</option>
                )}
              </select>
            </div>

            <div className="form-group">
              <label>Leverage</label>
              <div className="leverage-buttons">
                {[2, 5, 10, 20].map(lev => (
                  <button
                    key={lev}
                    className={leverage === lev ? 'active' : ''}
                    onClick={() => setLeverage(lev)}
                  >
                    {lev}x
                  </button>
                ))}
              </div>
            </div>

            <div className="form-group">
              <label>Margin: {marginPercent}% ({formatUsd(estimatedCollateral)})</label>
              <input
                type="range"
                min="1"
                max="100"
                value={marginPercent}
                onChange={e => setMarginPercent(Number(e.target.value))}
                className="margin-slider"
              />
              <div className="slider-labels">
                <span>1%</span>
                <span>25%</span>
                <span>50%</span>
                <span>75%</span>
                <span>100%</span>
              </div>
            </div>

            <div className="estimate">
              <div className="estimate-row">
                <span>Position Size:</span>
                <span>{formatUsd(estimatedSize)}</span>
              </div>
              <div className="estimate-row">
                <span>Tokens:</span>
                <span>~{estimatedQty.toFixed(4)} {selectedSymbol.split('-')[0]}</span>
              </div>
              <div className="estimate-row">
                <span>Collateral:</span>
                <span>{formatUsd(estimatedCollateral)}</span>
              </div>
              <div className="estimate-row">
                <span>Current Price:</span>
                <span>{formatPrice(midPrice)}</span>
              </div>
            </div>

            <div className="trade-buttons">
              <button
                className="btn-long"
                onClick={() => openPosition('long')}
                disabled={!canTrade}
              >
                📈 LONG
              </button>
              <button
                className="btn-short"
                onClick={() => openPosition('short')}
                disabled={!canTrade}
              >
                📉 SHORT
              </button>
            </div>
            {!canTrade && connected && (
              <div className="trade-warning">
                {midPrice <= 0 ? 'Waiting for price data...' : 'Insufficient balance'}
              </div>
            )}
          </div>

          {/* My Positions */}
          <div className="my-positions">
            <h3>My Positions</h3>
            {humanPositions.length === 0 ? (
              <div className="no-positions">No open positions</div>
            ) : (
              humanPositions.map(pos => {
                const pnl = formatPnl(pos.unrealized_pnl);
                const side = pos.side === 'buy' ? 'LONG' : 'SHORT';
                return (
                  <div key={`${pos.symbol}-${pos.side}`} className="position-card">
                    <div className="position-header">
                      <span className={`side ${pos.side}`}>{side}</span>
                      <span className="symbol">{pos.symbol}</span>
                      <span className="leverage">{pos.leverage_actual}x</span>
                    </div>
                    <div className="position-details">
                      <div className="detail-row">
                        <span>Size:</span>
                        <span>{formatUsd(pos.size_usd)}</span>
                      </div>
                      <div className="detail-row">
                        <span>Entry:</span>
                        <span>{formatPrice(pos.entry_price)}</span>
                      </div>
                      <div className="detail-row">
                        <span>Current:</span>
                        <span>{formatPrice(pos.current_price)}</span>
                      </div>
                      <div className="detail-row">
                        <span>Liq. Price:</span>
                        <span className="liq-price">{formatPrice(pos.liquidation_price)}</span>
                      </div>
                      <div className={`pnl-row ${pnl.positive ? 'positive' : 'negative'}`}>
                        <span>PnL:</span>
                        <span className="pnl">{pnl.text}</span>
                      </div>
                    </div>
                    {pos.is_liquidatable && (
                      <div className="liquidation-warning">⚠️ LIQUIDATION RISK</div>
                    )}
                    <button
                      className="btn-close"
                      onClick={() => closePosition(pos.symbol, pos.side)}
                    >
                      Close Position
                    </button>
                  </div>
                );
              })
            )}
          </div>
        </section>
      </main>
    </div>
  );
}

export default App;
