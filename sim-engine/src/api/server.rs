//! HTTP API server for external interaction with the simulation.

use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use tiny_http::{Method, Response, Server};

/// Command sent from HTTP API to HumanAgent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCommand {
    pub action: String,      // "open", "close", "status", "liquidations", "positions"
    pub symbol: String,
    pub side: Option<String>, // "long" or "short"
    pub qty: Option<f64>,     // Number of tokens as float (e.g., 0.5, 2.0)
    pub leverage: Option<u32>,
}

/// Response from HumanAgent back to HTTP API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// API Server handle
pub struct ApiServer {
    shutdown: Arc<AtomicBool>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl ApiServer {
    /// Start the API server on the given port.
    pub fn start(port: u16, response_rx: Receiver<ApiResponse>) -> (Self, Sender<ApiCommand>, Receiver<ApiCommand>) {
        // Bounded channel to prevent memory leak if HumanAgent is slow
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<ApiCommand>(100);
        let server = Self::start_with_channel(port, response_rx, cmd_tx.clone());
        (server, cmd_tx, cmd_rx)
    }

    /// Start the API server with an existing command channel
    pub fn start_with_channel(
        port: u16,
        response_rx: Receiver<ApiResponse>,
        cmd_tx: Sender<ApiCommand>,
    ) -> Self {
        let cmd_tx_clone = cmd_tx.clone();
        
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        let thread_handle = thread::spawn(move || {
            let addr = format!("0.0.0.0:{}", port);
            let server = match Server::http(&addr) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[ApiServer] Failed to bind to {}: {}", addr, e);
                    return;
                }
            };

            println!("[ApiServer] listening on http://{}", addr);
            
            loop {
                if shutdown_clone.load(Ordering::Relaxed) {
                    break;
                }

                let request = match server.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(Some(req)) => req,
                    Ok(None) => continue,
                    Err(e) => {
                        eprintln!("[ApiServer] Error: {}", e);
                        continue;
                    }
                };

                let method = request.method().clone();
                let url = request.url().to_string();

                match (method, url.as_str()) {
                    (Method::Post, "/order") => {
                        handle_order_request(request, &cmd_tx_clone, &response_rx);
                    }
                    (Method::Post, "/close") => {
                        handle_close_request(request, &cmd_tx_clone, &response_rx);
                    }
                    (Method::Post, "/preview") => {
                        handle_preview_request(request, &cmd_tx_clone, &response_rx);
                    }
                    (Method::Get, "/status") => {
                        handle_status_request(request, &cmd_tx_clone, &response_rx);
                    }
                    (Method::Get, "/health") => {
                        send_json_response(request, &ApiResponse {
                            success: true,
                            message: "OK".to_string(),
                            data: None,
                        });
                    }
                    _ => {
                        send_json_response(request, &ApiResponse {
                            success: false,
                            message: format!("Unknown endpoint: {}", url),
                            data: None,
                        });
                    }
                }
            }
        });

        Self {
            shutdown,
            thread_handle: Some(thread_handle),
        }
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn send_json_response(request: tiny_http::Request, resp: &ApiResponse) {
    let response = serde_json::to_string(resp).unwrap_or_else(|_| "{}".to_string());
    let http_response = Response::from_string(response)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
    let _ = request.respond(http_response);
}

fn handle_order_request(
    mut request: tiny_http::Request,
    cmd_tx: &Sender<ApiCommand>,
    response_rx: &Receiver<ApiResponse>,
) {
    let mut body = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut request.as_reader(), &mut body) {
        send_json_response(request, &ApiResponse {
            success: false,
            message: format!("Failed to read body: {}", e),
            data: None,
        });
        return;
    }

    let cmd: ApiCommand = match serde_json::from_str(&body) {
        Ok(c) => c,
        Err(e) => {
            send_json_response(request, &ApiResponse {
                success: false,
                message: format!("Invalid JSON: {}", e),
                data: None,
            });
            return;
        }
    };

    if let Err(e) = cmd_tx.send(cmd) {
        send_json_response(request, &ApiResponse {
            success: false,
            message: format!("Failed to send command: {}", e),
            data: None,
        });
        return;
    }

    let resp = match response_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(resp) => resp,
        Err(_) => ApiResponse {
            success: false,
            message: "Timeout waiting for response".to_string(),
            data: None,
        },
    };
    send_json_response(request, &resp);
}

fn handle_close_request(
    mut request: tiny_http::Request,
    cmd_tx: &Sender<ApiCommand>,
    response_rx: &Receiver<ApiResponse>,
) {
    let mut body = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut request.as_reader(), &mut body) {
        send_json_response(request, &ApiResponse {
            success: false,
            message: format!("Failed to read body: {}", e),
            data: None,
        });
        return;
    }

    #[derive(Deserialize)]
    struct CloseRequest { symbol: String }

    let close_req: CloseRequest = match serde_json::from_str(&body) {
        Ok(c) => c,
        Err(e) => {
            send_json_response(request, &ApiResponse {
                success: false,
                message: format!("Invalid JSON: {}", e),
                data: None,
            });
            return;
        }
    };

    let cmd = ApiCommand {
        action: "close".to_string(),
        symbol: close_req.symbol,
        side: None,
        qty: None,
        leverage: None,
    };

    if let Err(e) = cmd_tx.send(cmd) {
        send_json_response(request, &ApiResponse {
            success: false,
            message: format!("Failed to send command: {}", e),
            data: None,
        });
        return;
    }

    let resp = match response_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(resp) => resp,
        Err(_) => ApiResponse {
            success: false,
            message: "Timeout".to_string(),
            data: None,
        },
    };
    send_json_response(request, &resp);
}

fn handle_preview_request(
    mut request: tiny_http::Request,
    cmd_tx: &Sender<ApiCommand>,
    response_rx: &Receiver<ApiResponse>,
) {
    let mut body = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut request.as_reader(), &mut body) {
        send_json_response(request, &ApiResponse {
            success: false,
            message: format!("Failed to read body: {}", e),
            data: None,
        });
        return;
    }

    let mut cmd: ApiCommand = match serde_json::from_str(&body) {
        Ok(c) => c,
        Err(e) => {
            send_json_response(request, &ApiResponse {
                success: false,
                message: format!("Invalid JSON: {}", e),
                data: None,
            });
            return;
        }
    };

    cmd.action = "preview".to_string();

    if let Err(e) = cmd_tx.send(cmd) {
        send_json_response(request, &ApiResponse {
            success: false,
            message: format!("Failed to send command: {}", e),
            data: None,
        });
        return;
    }

    let resp = match response_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(resp) => resp,
        Err(_) => ApiResponse {
            success: false,
            message: "Timeout waiting for response".to_string(),
            data: None,
        },
    };
    send_json_response(request, &resp);
}

fn handle_status_request(
    request: tiny_http::Request,
    cmd_tx: &Sender<ApiCommand>,
    response_rx: &Receiver<ApiResponse>,
) {
    let cmd = ApiCommand {
        action: "status".to_string(),
        symbol: "".to_string(),
        side: None,
        qty: None,
        leverage: None,
    };

    if let Err(e) = cmd_tx.send(cmd) {
        send_json_response(request, &ApiResponse {
            success: false,
            message: format!("Failed: {}", e),
            data: None,
        });
        return;
    }

    let resp = match response_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(resp) => resp,
        Err(_) => ApiResponse {
            success: false,
            message: "Timeout".to_string(),
            data: None,
        },
    };
    send_json_response(request, &resp);
}

