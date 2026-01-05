use crate::api::{ApiCommand, ApiResponse};
use crate::events::SimEvent;
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use tungstenite::accept;
use tungstenite::Message;

/// WebSocket Server handle
pub struct WsServer {
    /// Shutdown signal for graceful termination (reserved for future use)
    #[allow(dead_code)]
    shutdown: Arc<AtomicBool>,
    /// Thread handle for joining on shutdown (reserved for future use)
    #[allow(dead_code)]
    thread_handle: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
enum WsMessage {
    /// Command from client
    Command(ApiCommand),
    /// Event from server
    Event(SimEvent),
    /// Response to command
    Response(ApiResponse),
    /// Error message
    Error(String),
}

impl WsServer {
    /// Start the WebSocket server on the given port
    pub fn start(
        port: u16,
        cmd_tx: Sender<ApiCommand>,
        event_rx: Receiver<SimEvent>,
        response_rx: Receiver<ApiResponse>,
    ) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        // Shared list of active WebSocket sockets
        // Using Arc<Mutex<Vec<Sender<Message>>>> logic would be better to avoid blocking everything
        // But tungstenite::WebSocket is not thread-safe and blocking.
        // Simplified approach:
        // 1. Accept thread: accepts connections and spawns a thread per client.
        // 2. Client thread: reads from socket -> sends to cmd_tx.
        // 3. Broadcast thread: reads from event_rx -> sends to all clients?
        //    Problem: How to send to client socket from another thread if the socket is blocked in read()?
        //    Solution: We need a channel per client. The client thread selects on (socket_read, client_channel_rx).
        //    Since tungstenite is blocking, we can't easily select.
        //    Alternative: Split socket into reader and writer? tungstenite supports this via verify_protocol? No, `WebSocket` has `split()`.
        
        // Actually, let's use a simpler approach for this prototype:
        // We will have a list of "Client Senders" (channels) protected by Mutex.
        // The Broadcast Thread reads event_rx and sends to all Client Senders.
        // Each Client Thread has its own loop that writes messages from its channel to the socket.
        // AND it needs to read from socket.
        // To do full duplex with blocking sockets, we need to split the socket or spawn 2 threads per client.
        // Or just use non-blocking mode, but that complicates things.
        
        // Let's spawn 2 threads per client: Reader and Writer.
        
        let clients: Arc<Mutex<Vec<Sender<String>>>> = Arc::new(Mutex::new(Vec::new()));

        let thread_handle = thread::spawn(move || {
            let addr = format!("0.0.0.0:{}", port);
            let listener = match TcpListener::bind(&addr) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[WsServer] Failed to bind to {}: {}", addr, e);
                    return;
                }
            };

            println!("[WsServer] Listening on ws://{}", addr);

            // Thread for broadcasting events
            let broadcast_clients = clients.clone();
            thread::spawn(move || {
                println!("[WsServer] Broadcast thread started, waiting for events...");
                // Loop to handle events
                while let Ok(event) = event_rx.recv() {
                    println!("[WsServer] Broadcasting event: {:?}", std::mem::discriminant(&event));
                    let json = match serde_json::to_string(&WsMessage::Event(event)) {
                        Ok(j) => j,
                        Err(e) => {
                            eprintln!("[WsServer] Serialization error: {}", e);
                            continue;
                        }
                    };
                    
                    broadcast_to_all(&broadcast_clients, &json);
                }
                println!("[WsServer] Broadcast thread exiting");
            });

            // Thread for broadcasting command responses (from HumanAgent)
            let response_clients = clients.clone();
            thread::spawn(move || {
                while let Ok(resp) = response_rx.recv() {
                     let json = match serde_json::to_string(&WsMessage::Response(resp)) {
                        Ok(j) => j,
                        Err(e) => {
                            eprintln!("[WsServer] Serialization error: {}", e);
                            continue;
                        }
                    };
                    broadcast_to_all(&response_clients, &json);
                }
            });

            // Accept loop
            for stream in listener.incoming() {
                if shutdown_clone.load(Ordering::Relaxed) {
                    break;
                }

                match stream {
                    Ok(stream) => {
                        let clients_inner = clients.clone();
                        let cmd_tx_inner = cmd_tx.clone();
                        
                        thread::spawn(move || {
                            let peer_addr = stream.peer_addr().unwrap();
                            println!("[WsServer] New connection: {}", peer_addr);

                            let mut websocket = match accept(stream) {
                                Ok(ws) => ws,
                                Err(e) => {
                                    eprintln!("[WsServer] Handshake error: {}", e);
                                    return;
                                }
                            };

                            // Channel to send messages to this client
                            let (tx, rx) = crossbeam_channel::unbounded::<String>();
                            
                            // Add to clients list
                            {
                                let mut guard = clients_inner.lock().unwrap();
                                guard.push(tx);
                            }

                            // We need to handle both reading and writing. 
                            // Since `websocket` is a single object, we can't easily split it across threads without `Arc<Mutex>` which kills concurrency 
                            // or using `start_threading` features if available, but standard tungstenite is simple.
                            // 
                            // BUT: Simpler trick: Set socket to non-blocking? Or use short timeouts.
                            // Tungstenite allows configuring stream.
                            
                            let stream_ref = websocket.get_ref();
                            stream_ref.set_read_timeout(Some(std::time::Duration::from_millis(100))).ok();

                            loop {
                                // 1. Try to read from channel and write to socket
                                let mut wrote = false;
                                while let Ok(msg) = rx.try_recv() {
                                    if let Err(e) = websocket.send(Message::Text(msg.into())) {
                                        eprintln!("[WsServer] Write error to {}: {}", peer_addr, e);
                                        return; // Disconnect
                                    }
                                    wrote = true;
                                }
                                if wrote {
                                    if let Err(e) = websocket.flush() {
                                        eprintln!("[WsServer] Flush error to {}: {}", peer_addr, e);
                                        return;
                                    }
                                }
                                
                                // 2. Try to read from socket (with timeout)
                                match websocket.read() {
                                    Ok(msg) => {
                                        if msg.is_text() || msg.is_binary() {
                                            let text = msg.to_string();
                                            // Try to parse as ApiCommand
                                            match serde_json::from_str::<ApiCommand>(&text) {
                                                Ok(cmd) => {
                                                    println!("[WsServer] Cmd from {}: {:?}", peer_addr, cmd);
                                                    let _ = cmd_tx_inner.send(cmd);
                                                },
                                                Err(e) => {
                                                    // Maybe it's wrapped in WsMessage::Command?
                                                    // Or just raw JSON?
                                                    // Let's try raw JSON first as per previous API.
                                                    eprintln!("[WsServer] Invalid command from {}: {}", peer_addr, e);
                                                    let err_msg = json!({
                                                        "type": "Error",
                                                        "payload": format!("Invalid command: {}", e)
                                                    }).to_string();
                                                    let _ = websocket.write(Message::Text(err_msg.into()));
                                                }
                                            }
                                        } else if msg.is_close() {
                                            println!("[WsServer] Client {} closed", peer_addr);
                                            break;
                                        }
                                    }
                                    Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                                        // Timeout, just continue loop
                                    }
                                    Err(e) => {
                                        eprintln!("[WsServer] Read error from {}: {}", peer_addr, e);
                                        break;
                                    }
                                }
                                
                                // Small sleep to prevent busy loop if timeout is small? 
                                // No, read() blocks for 100ms, so it's fine.
                            }
                            
                            // Cleanup: Remove tx from clients list?
                            // Implementing robust removal is tricky with Vec<Sender>.
                            // For a sim demo, we can live with dead channels accumulating in the list 
                            // (channel send will fail and we can handle it in broadcast loop, but we aren't handling it there yet).
                            // Let's improve broadcast loop.
                        });
                    }
                    Err(e) => {
                        eprintln!("[WsServer] Connection accept error: {}", e);
                    }
                }
            }
        });

        Self {
            shutdown,
            thread_handle: Some(thread_handle),
        }
    }
}

fn broadcast_to_all(clients: &Arc<Mutex<Vec<Sender<String>>>>, msg: &str) {
    let mut guard = clients.lock().unwrap();
    // Retain only active clients (those where send succeeds)
    guard.retain(|tx| {
        tx.send(msg.to_string()).is_ok()
    });
}

