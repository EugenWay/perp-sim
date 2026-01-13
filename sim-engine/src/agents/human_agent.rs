//! HumanAgent - receives commands from HTTP API and executes them in the simulation.

use crossbeam_channel::{Receiver, Sender};

use crate::agents::Agent;
use crate::api::{ApiCommand, ApiResponse};
use crate::messages::{
    AgentId, CloseOrderPayload, MarketOrderPayload, Message, MessagePayload, MessageType,
    OrderExecutedPayload, OrderExecutionType, PositionLiquidatedPayload, Side, SimulatorApi,
};

/// Initial balance for Human trader (in micro-USD = $10,000)
const INITIAL_BALANCE: i128 = 10_000_000_000;

pub struct HumanAgent {
    id: AgentId,
    name: String,
    exchange_id: AgentId,
    command_rx: Receiver<ApiCommand>,
    response_tx: Sender<ApiResponse>,
    wake_interval_ns: u64,
    open_positions: std::collections::HashMap<String, Side>,
    /// Total collateral locked in open positions
    collateral_used: i128,
    /// Current balance (updated with PnL from closed positions)
    balance: i128,
    /// Total realized PnL
    total_pnl: i128,
}

impl HumanAgent {
    pub fn new(
        id: AgentId,
        name: String,
        exchange_id: AgentId,
        command_rx: Receiver<ApiCommand>,
        response_tx: Sender<ApiResponse>,
        wake_interval_ms: u64,
    ) -> Self {
        Self {
            id,
            name,
            exchange_id,
            command_rx,
            response_tx,
            wake_interval_ns: wake_interval_ms * 1_000_000,
            open_positions: std::collections::HashMap::new(),
            collateral_used: 0,
            balance: INITIAL_BALANCE,
            total_pnl: 0,
        }
    }

    /// Get available balance (balance - collateral used)
    pub fn available_balance(&self) -> i128 {
        self.balance - self.collateral_used
    }

    fn process_commands(&mut self, sim: &mut dyn SimulatorApi) {
        while let Ok(cmd) = self.command_rx.try_recv() {
            println!("[{}] cmd: {} {}", self.name, cmd.action, cmd.symbol);

            let response = match cmd.action.as_str() {
                "open" | "order" => self.handle_open(sim, &cmd),
                "close" => self.handle_close(sim, &cmd),
                "status" => self.handle_status(),
                "balance" => self.handle_balance(),
                _ => ApiResponse {
                    success: false,
                    message: format!("Unknown action: {}", cmd.action),
                    data: None,
                },
            };

            let _ = self.response_tx.send(response);
        }
    }

    fn handle_open(&mut self, sim: &mut dyn SimulatorApi, cmd: &ApiCommand) -> ApiResponse {
        let side = match cmd.side.as_deref() {
            Some("long") | Some("buy") | Some("Long") | Some("Buy") => Side::Buy,
            Some("short") | Some("sell") | Some("Short") | Some("Sell") => Side::Sell,
            _ => return ApiResponse {
                success: false,
                message: "side must be 'long' or 'short'".to_string(),
                data: None,
            },
        };

        let qty = cmd.qty.unwrap_or(1.0);
        let leverage = cmd.leverage.unwrap_or(5);

        sim.send(
            self.id,
            self.exchange_id,
            MessageType::MarketOrder,
            MessagePayload::MarketOrder(MarketOrderPayload {
                symbol: cmd.symbol.clone(),
                side,
                qty,
                leverage,
            }),
        );

        self.open_positions.insert(cmd.symbol.clone(), side);

        ApiResponse {
            success: true,
            message: format!("Order: {} {:?} qty={:.2} lev={}x", cmd.symbol, side, qty, leverage),
            data: Some(serde_json::json!({
                "symbol": cmd.symbol,
                "side": format!("{:?}", side),
                "qty": qty,
                "leverage": leverage,
            })),
        }
    }

    fn handle_close(&mut self, sim: &mut dyn SimulatorApi, cmd: &ApiCommand) -> ApiResponse {
        let side = match self.open_positions.get(&cmd.symbol) {
            Some(s) => *s,
            None => return ApiResponse {
                success: false,
                message: format!("No open position for {}", cmd.symbol),
                data: None,
            },
        };

        sim.send(
            self.id,
            self.exchange_id,
            MessageType::CloseOrder,
            MessagePayload::CloseOrder(CloseOrderPayload {
                symbol: cmd.symbol.clone(),
                side,
            }),
        );

        self.open_positions.remove(&cmd.symbol);

        ApiResponse {
            success: true,
            message: format!("Close: {} ({:?})", cmd.symbol, side),
            data: None,
        }
    }

    fn handle_status(&self) -> ApiResponse {
        let positions: Vec<_> = self.open_positions.iter()
            .map(|(s, side)| serde_json::json!({"symbol": s, "side": format!("{:?}", side)}))
            .collect();

        ApiResponse {
            success: true,
            message: format!("{} positions", positions.len()),
            data: Some(serde_json::json!({"agent": self.name, "positions": positions})),
        }
    }

    fn handle_balance(&self) -> ApiResponse {
        let available = self.available_balance();
        ApiResponse {
            success: true,
            message: format!("Balance: ${:.2}", available as f64 / 1_000_000.0),
            data: Some(serde_json::json!({
                "balance": self.balance,
                "collateral_used": self.collateral_used,
                "available_balance": available,
                "total_pnl": self.total_pnl,
            })),
        }
    }

    fn handle_order_executed(&mut self, payload: &OrderExecutedPayload) {
        match payload.order_type {
            OrderExecutionType::Increase => {
                // Opening position - lock collateral
                self.collateral_used += payload.collateral_delta;
                println!(
                    "[{}] Position opened: {} {:?} size=${:.2} collateral=${:.2}",
                    self.name,
                    payload.symbol,
                    payload.side,
                    payload.size_usd as f64 / 1_000_000.0,
                    payload.collateral_delta as f64 / 1_000_000.0
                );
            }
            OrderExecutionType::Decrease => {
                // Closing position - return collateral and apply PnL
                // collateral_delta is negative (returned)
                self.collateral_used += payload.collateral_delta; // Adds negative = decreases
                self.balance += payload.pnl;
                self.total_pnl += payload.pnl;
                println!(
                    "[{}] Position closed: {} {:?} pnl=${:.2} balance=${:.2}",
                    self.name,
                    payload.symbol,
                    payload.side,
                    payload.pnl as f64 / 1_000_000.0,
                    self.balance as f64 / 1_000_000.0
                );
            }
            OrderExecutionType::Liquidation => {
                // Liquidation - collateral is lost
                self.collateral_used += payload.collateral_delta;
                self.balance += payload.pnl;
                self.total_pnl += payload.pnl;
                println!(
                    "[{}] ⚠️ Liquidated: {} {:?} pnl=${:.2}",
                    self.name,
                    payload.symbol,
                    payload.side,
                    payload.pnl as f64 / 1_000_000.0
                );
            }
        }
    }
}

impl Agent for HumanAgent {
    fn id(&self) -> AgentId { self.id }
    fn name(&self) -> &str { &self.name }

    fn on_start(&mut self, sim: &mut dyn SimulatorApi) {
        println!("[{}] ====== STARTED id={} balance=${:.2} ======", self.name, self.id, self.balance as f64 / 1_000_000.0);
        sim.wakeup(self.id, sim.now_ns() + self.wake_interval_ns);
    }

    fn on_wakeup(&mut self, sim: &mut dyn SimulatorApi, _now_ns: u64) {
        // Check for pending commands
        let pending = self.command_rx.len();
        if pending > 0 {
            println!("[{}] Processing {} pending commands", self.name, pending);
        }
        self.process_commands(sim);
        sim.wakeup(self.id, sim.now_ns() + self.wake_interval_ns);
    }

    fn on_message(&mut self, _sim: &mut dyn SimulatorApi, msg: &Message) {
        match msg.msg_type {
            MessageType::OrderAccepted | MessageType::OrderRejected => {
                println!("[{}] received {:?}", self.name, msg.msg_type);
            }
            MessageType::OrderExecuted => {
                if let MessagePayload::OrderExecuted(payload) = &msg.payload {
                    self.handle_order_executed(payload);
                }
            }
            MessageType::PositionLiquidated => {
                if let MessagePayload::PositionLiquidated(PositionLiquidatedPayload { symbol, side, pnl, collateral_lost, .. }) = &msg.payload {
                    let side_str = match side {
                        Side::Buy => "LONG",
                        Side::Sell => "SHORT",
                    };
                    println!(
                        "[{}] ⚠️ LIQUIDATED {} {} pnl=${:.2} lost=${:.2}",
                        self.name, symbol, side_str,
                        *pnl as f64 / 1_000_000.0,
                        *collateral_lost as f64 / 1_000_000.0
                    );
                    // Update balance
                    self.balance += *pnl;
                    self.total_pnl += *pnl;
                    self.collateral_used -= *collateral_lost;
                    // Remove position tracking
                    self.open_positions.remove(symbol);
                }
            }
            _ => {}
        }
    }
}

