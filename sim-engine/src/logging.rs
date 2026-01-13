// Simple CSV loggers on top of EventBus.

use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::events::{EventListener, SimEvent};

fn open_csv_with_header(dir: &Path, filename: &str, header: &str) -> std::io::Result<std::fs::File> {
    create_dir_all(dir)?;
    let path: PathBuf = dir.join(filename);

    let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(path)?;

    // Write header immediately.
    file.write_all(header.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(file)
}

/// Order logger: logs/orders.csv
pub struct CsvOrderLogger {
    file: std::fs::File,
}

impl CsvOrderLogger {
    pub fn new<P: AsRef<Path>>(dir: P) -> std::io::Result<Self> {
        let header = "ts,from,to,msg_type,symbol,side,price,qty";
        let file = open_csv_with_header(dir.as_ref(), "orders.csv", header)?;
        Ok(Self { file })
    }
}

impl EventListener for CsvOrderLogger {
    fn on_event(&mut self, event: &SimEvent) {
        if let SimEvent::OrderLog {
            ts,
            from,
            to,
            msg_type,
            symbol,
            side,
            price,
            qty,
        } = event
        {
            let symbol_str = symbol.as_deref().unwrap_or("");
            let side_str = side.map(|s| format!("{:?}", s)).unwrap_or_default();
            let price_str = price.map(|p| p.to_string()).unwrap_or_default();
            let qty_str = qty.map(|q| q.to_string()).unwrap_or_default();

            let line = format!(
                "{ts},{from},{to},{msg_type:?},{symbol},{side},{price},{qty}\n",
                ts = ts,
                from = from,
                to = to,
                msg_type = msg_type,
                symbol = symbol_str,
                side = side_str,
                price = price_str,
                qty = qty_str,
            );

            if let Err(e) = self.file.write_all(line.as_bytes()) {
                eprintln!("[CsvOrderLogger] write error: {e}");
            }
        }
    }
}

/// Oracle logger: logs/oracle.csv
pub struct CsvOracleLogger {
    file: std::fs::File,
}

impl CsvOracleLogger {
    pub fn new<P: AsRef<Path>>(dir: P) -> std::io::Result<Self> {
        let header = "ts,symbol,price_min,price_max,price_mid";
        let file = open_csv_with_header(dir.as_ref(), "oracle.csv", header)?;
        Ok(Self { file })
    }
}

impl EventListener for CsvOracleLogger {
    fn on_event(&mut self, event: &SimEvent) {
        if let SimEvent::OracleTick {
            ts,
            symbol,
            price_min,
            price_max,
        } = event
        {
            let price_mid = (price_min + price_max) / 2;
            let line = format!("{ts},{symbol},{price_min},{price_max},{price_mid}\n");
            if let Err(e) = self.file.write_all(line.as_bytes()) {
                eprintln!("[CsvOracleLogger] write error: {e}");
            }
        }
    }
}

/// Execution logger: logs/executions.csv
pub struct CsvExecutionLogger {
    file: std::fs::File,
}

impl CsvExecutionLogger {
    pub fn new<P: AsRef<Path>>(dir: P) -> std::io::Result<Self> {
        let header = "ts,account,symbol,side,size_usd,collateral,execution_price,leverage,order_type,pnl";
        let file = open_csv_with_header(dir.as_ref(), "executions.csv", header)?;
        Ok(Self { file })
    }
}

impl EventListener for CsvExecutionLogger {
    fn on_event(&mut self, event: &SimEvent) {
        if let SimEvent::OrderExecuted {
            ts,
            account,
            symbol,
            side,
            size_usd,
            collateral,
            execution_price,
            leverage,
            order_type,
            pnl,
        } = event
        {
            let side_str = format!("{:?}", side);
            let line = format!(
                "{ts},{account},{symbol},{side},{size_usd},{collateral},{execution_price},{leverage},{order_type},{pnl}\n",
                ts = ts,
                account = account,
                symbol = symbol,
                side = side_str,
                size_usd = size_usd,
                collateral = collateral,
                execution_price = execution_price,
                leverage = leverage,
                order_type = order_type,
                pnl = pnl,
            );

            if let Err(e) = self.file.write_all(line.as_bytes()) {
                eprintln!("[CsvExecutionLogger] write error: {e}");
            }
        }
    }
}

/// Position snapshot logger: logs/positions.csv
pub struct CsvPositionLogger {
    file: std::fs::File,
}

impl CsvPositionLogger {
    pub fn new<P: AsRef<Path>>(dir: P) -> std::io::Result<Self> {
        let header = "ts,account,symbol,side,size_usd,size_tokens,collateral,entry_price,current_price,pnl,liquidation_price,leverage,is_liquidatable,opened_at";
        let file = open_csv_with_header(dir.as_ref(), "positions.csv", header)?;
        Ok(Self { file })
    }
}

impl EventListener for CsvPositionLogger {
    fn on_event(&mut self, event: &SimEvent) {
        if let SimEvent::PositionSnapshot {
            ts,
            account,
            symbol,
            side,
            size_usd,
            size_tokens,
            collateral,
            entry_price,
            current_price,
            unrealized_pnl,
            liquidation_price,
            leverage_actual,
            is_liquidatable,
            opened_at_sec,
        } = event
        {
            let side_str = format!("{:?}", side);
            let line = format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                ts,
                account,
                symbol,
                side_str,
                size_usd,
                size_tokens,
                collateral,
                entry_price,
                current_price,
                unrealized_pnl,
                liquidation_price,
                leverage_actual,
                is_liquidatable,
                opened_at_sec,
            );

            if let Err(e) = self.file.write_all(line.as_bytes()) {
                eprintln!("[CsvPositionLogger] write error: {e}");
            }
        }
    }
}

/// Market state logger: logs/markets.csv
pub struct CsvMarketLogger {
    file: std::fs::File,
}

impl CsvMarketLogger {
    pub fn new<P: AsRef<Path>>(dir: P) -> std::io::Result<Self> {
        let header = "ts,symbol,oi_long_usd,oi_short_usd,liquidity_usd";
        let file = open_csv_with_header(dir.as_ref(), "markets.csv", header)?;
        Ok(Self { file })
    }
}

impl EventListener for CsvMarketLogger {
    fn on_event(&mut self, event: &SimEvent) {
        if let SimEvent::MarketSnapshot {
            ts,
            symbol,
            oi_long_usd,
            oi_short_usd,
            liquidity_usd,
        } = event
        {
            let line = format!(
                "{},{},{},{},{}\n",
                ts, symbol, oi_long_usd, oi_short_usd, liquidity_usd,
            );

            if let Err(e) = self.file.write_all(line.as_bytes()) {
                eprintln!("[CsvMarketLogger] write error: {e}");
            }
        }
    }
}

/// Liquidation logger: logs/liquidations.csv
pub struct CsvLiquidationLogger {
    file: std::fs::File,
}

impl CsvLiquidationLogger {
    pub fn new<P: AsRef<Path>>(dir: P) -> std::io::Result<Self> {
        let header = "ts,account,symbol,side,size_usd,collateral_lost,pnl,liquidation_price";
        let file = open_csv_with_header(dir.as_ref(), "liquidations.csv", header)?;
        Ok(Self { file })
    }
}

impl EventListener for CsvLiquidationLogger {
    fn on_event(&mut self, event: &SimEvent) {
        if let SimEvent::PositionLiquidated {
            ts,
            account,
            symbol,
            side,
            size_usd,
            collateral_lost,
            pnl,
            liquidation_price,
        } = event
        {
            let side_str = format!("{:?}", side);
            let line = format!(
                "{},{},{},{},{},{},{},{}\n",
                ts,
                account,
                symbol,
                side_str,
                size_usd,
                collateral_lost,
                pnl,
                liquidation_price,
            );

            if let Err(e) = self.file.write_all(line.as_bytes()) {
                eprintln!("[CsvLiquidationLogger] write error: {e}");
            }
        }
    }
}
