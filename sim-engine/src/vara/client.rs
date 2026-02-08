use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gclient::{GearApi, WSAddress};
use primitive_types::U256;
use sails_rs::client::{Actor, GclientEnv};
use sp_core::crypto::{AccountId32, Ss58Codec};
use tokio::runtime::Runtime;
use tokio::sync::RwLock;

/// 32-byte hash type (message ID, block hash, etc.)
pub type H256 = [u8; 32];

/// Macro to run a read-only query through the VaraPerps service.
/// Eliminates boilerplate: inner_ref -> block_on -> read lock -> env -> actor -> service.
///
/// Usage: `query!(self, |service| service.get_order(id).query().await.map_err(...))`
macro_rules! query {
    ($self:expr, |$s:ident| $body:expr) => {{
        let inner = $self.inner_ref()?;
        $self.runtime.block_on(async {
            let guard = inner.read().await;
            let env = GclientEnv::new(guard.api.clone());
            let actor = Actor::<VaraPerpsProgram, GclientEnv>::new(env, guard.contract_id.into());
            let $s = actor.vara_perps();
            $body
        })
    }};
}

/// Macro to run a fire-and-forget transaction on the bounded blocking thread pool.
/// Handles the common shell: inner clone, agent lock, spawn_blocking, runtime build,
/// keypair load, error reporting. The body receives (keypair, api, contract_id, gas_limit, tx_sender).
///
/// Usage:
/// ```ignore
/// fire_and_forget!(self, agent_id, TxType::SubmitOrder, |kp, api, cid, gas, tx| {
///     // ... async code using kp, api, cid, gas; send result via tx ...
/// });
/// ```
macro_rules! fire_and_forget {
    ($self:expr, $agent_id:expr, $tx_type:expr, $( $captures:ident ),* , |$kp:ident, $api:ident, $cid:ident, $gas:ident, $tx:ident| $body:expr) => {{
        let inner = $self.inner_ref()?.clone();
        let lock = $self.agent_lock($agent_id);
        let tx_sender = $self.tx_result_tx.clone();
        let agent_id = $agent_id;
        $( let $captures = $captures; )*

        $self.runtime.handle().spawn_blocking(move || {
            let _guard = lock.lock().unwrap();
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[Vara] {}: runtime error: {}", stringify!($tx_type), e);
                    let _ = tx_sender.send(TxResult {
                        agent_id, tx_type: $tx_type, success: false,
                        order_id: None, error: Some(e.to_string()),
                        detail: "runtime build error".into(),
                    });
                    return;
                }
            };
            rt.block_on(async move {
                let ($kp, $api, $cid, $gas) = {
                    let guard = inner.read().await;
                    let kp = match guard.keystore.load_keypair_for_agent(agent_id) {
                        Ok(kp) => kp.clone(),
                        Err(e) => {
                            eprintln!("[Vara] {}: keypair error: {}", stringify!($tx_type), e);
                            let _ = tx_sender.send(TxResult {
                                agent_id, tx_type: $tx_type, success: false,
                                order_id: None, error: Some(e.to_string()),
                                detail: "keypair error".into(),
                            });
                            return;
                        }
                    };
                    (kp, guard.api.clone(), guard.contract_id, guard.gas_limits)
                };
                let $tx = tx_sender;
                $body
            });
        });
    }};
}

use super::generated::VaraPerps as VaraPerpsTrait;
use super::generated::VaraPerpsProgram;
use super::generated::vara_perps::VaraPerps as _VaraPerpsServiceTrait; // trait must be in scope for service methods
use super::keystore::{KeystoreError, KeystoreManager};
use super::types::{
    ActorId, LiquidationPreview, OracleInput, Order, OrderId, Position, PositionKey,
    Side as VaraSide,
};

// ========== Transaction Result Feedback ==========

/// Type of on-chain transaction
#[derive(Debug, Clone)]
pub enum TxType {
    SubmitOrder,
    ExecuteOrder,
    CancelOrder,
    SubmitAndExecute,
}

impl std::fmt::Display for TxType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SubmitOrder => write!(f, "SubmitOrder"),
            Self::ExecuteOrder => write!(f, "ExecuteOrder"),
            Self::CancelOrder => write!(f, "CancelOrder"),
            Self::SubmitAndExecute => write!(f, "SubmitAndExecute"),
        }
    }
}

/// Result of an on-chain transaction, sent back through a channel
/// so the ExchangeAgent (and agents via messages) know the outcome.
#[derive(Debug, Clone)]
pub struct TxResult {
    /// Agent that initiated the transaction
    pub agent_id: u32,
    /// Type of transaction
    pub tx_type: TxType,
    /// Whether the transaction succeeded on-chain
    pub success: bool,
    /// On-chain order ID (from SubmitOrder reply)
    pub order_id: Option<u64>,
    /// Error message if failed
    pub error: Option<String>,
    /// Human-readable detail for logging
    pub detail: String,
}

/// Error type for Vara client operations
#[derive(Debug)]
pub enum VaraError {
    /// Connection error
    Connection(String),
    /// Keystore error
    Keystore(KeystoreError),
    /// Transaction error
    Transaction(String),
    /// Query error
    Query(String),
    /// Configuration error
    Config(String),
    /// Runtime error
    Runtime(String),
}

impl std::fmt::Display for VaraError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection(s) => write!(f, "Connection error: {}", s),
            Self::Keystore(e) => write!(f, "Keystore error: {}", e),
            Self::Transaction(s) => write!(f, "Transaction error: {}", s),
            Self::Query(s) => write!(f, "Query error: {}", s),
            Self::Config(s) => write!(f, "Config error: {}", s),
            Self::Runtime(s) => write!(f, "Runtime error: {}", s),
        }
    }
}

impl std::error::Error for VaraError {}

impl From<KeystoreError> for VaraError {
    fn from(e: KeystoreError) -> Self {
        VaraError::Keystore(e)
    }
}

impl From<gclient::Error> for VaraError {
    fn from(e: gclient::Error) -> Self {
        VaraError::Connection(e.to_string())
    }
}

/// Configuration for VaraClient
#[derive(Debug, Clone)]
pub struct VaraConfig {
    /// WebSocket endpoint (e.g., "wss://testnet.vara.network")
    pub ws_endpoint: String,
    /// Contract program ID (hex string)
    pub contract_address: String,
    /// Path to keystore directory
    pub keystore_path: String,
    /// Path to passphrase file
    pub passphrase_path: String,
    /// Block time in milliseconds (default: 3000)
    pub block_time_ms: u64,
    /// Gas limit for transactions
    pub gas_limit: u64,
}

impl VaraConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self, VaraError> {
        let contract_address = std::env::var("VARA_CONTRACT_ADDRESS")
            .map_err(|_| VaraError::Config("VARA_CONTRACT_ADDRESS not set".to_string()))?;

        let ws_endpoint =
            std::env::var("VARA_WS_ENDPOINT").unwrap_or_else(|_| "wss://testnet.vara.network".to_string());

        let keystore_path = std::env::var("VARA_KEYSTORE_PATH").unwrap_or_else(|_| {
            let local = "keys/Library/Application Support/gring";
            let parent = "../keys/Library/Application Support/gring";
            if std::path::Path::new(local).exists() {
                local.to_string()
            } else {
                parent.to_string()
            }
        });

        let passphrase_path = std::env::var("VARA_PASSPHRASE_PATH")
            .or_else(|_| std::env::var("VARA_PASSPHRASE_FILE"))
            .unwrap_or_else(|_| {
                let local = "keys/.passphrase";
                let parent = "../keys/.passphrase";
                if std::path::Path::new(local).exists() {
                    local.to_string()
                } else {
                    parent.to_string()
                }
            });

        let block_time_ms = std::env::var("VARA_BLOCK_TIME_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3000);

        let gas_limit = std::env::var("VARA_GAS_LIMIT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100_000_000_000u64); // 100B gas default

        Ok(Self {
            ws_endpoint,
            contract_address,
            keystore_path,
            passphrase_path,
            block_time_ms,
            gas_limit,
        })
    }

    /// Create config with explicit values
    pub fn new(
        ws_endpoint: impl Into<String>,
        contract_address: impl Into<String>,
        keystore_path: impl Into<String>,
        passphrase_path: impl Into<String>,
    ) -> Self {
        Self {
            ws_endpoint: ws_endpoint.into(),
            contract_address: contract_address.into(),
            keystore_path: keystore_path.into(),
            passphrase_path: passphrase_path.into(),
            block_time_ms: 3000,
            gas_limit: 100_000_000_000,
        }
    }
}

/// Per-operation gas limits.
/// Different contract methods have different computational costs.
#[derive(Debug, Clone, Copy)]
struct GasLimits {
    deposit: u64,
    withdraw: u64,
    add_liquidity: u64,
    submit_order: u64,
    execute_order: u64,
    cancel_order: u64,
}

impl GasLimits {
    /// Create from a single default value, then scale per operation.
    fn from_default(base: u64) -> Self {
        Self {
            deposit: base,             // simple balance update
            withdraw: base,            // simple balance update
            add_liquidity: base,       // simple balance update
            submit_order: base,        // moderate: validation + storage
            execute_order: base * 3 / 2, // heavy: price calc + position update + fees
            cancel_order: base / 2,    // light: just remove from storage
        }
    }
}

/// Shared state for VaraClient
struct VaraClientInner {
    /// GearApi connection
    api: GearApi,
    /// Contract program ID as bytes
    contract_id: [u8; 32],
    /// Keystore manager
    keystore: KeystoreManager,
    /// Per-operation gas limits
    gas_limits: GasLimits,
}

/// Vara Network client for interacting with VaraPerps contract
///
/// Thread-safe wrapper around gclient::GearApi with keystore integration.
/// All blockchain operations are executed asynchronously.
pub struct VaraClient {
    /// Configuration
    config: VaraConfig,
    /// Tokio runtime for async operations
    runtime: Runtime,
    /// Inner state (wrapped in RwLock for thread safety)
    inner: Option<Arc<RwLock<VaraClientInner>>>,
    /// Connection status
    connected: bool,
    /// Per-agent mutexes to serialize txs from the same account (prevents nonce collisions)
    agent_locks: Arc<Mutex<HashMap<u32, Arc<Mutex<()>>>>>,
    /// Channel sender for reporting fire-and-forget transaction results
    tx_result_tx: crossbeam_channel::Sender<TxResult>,
    /// Channel receiver (taken once by ExchangeAgent)
    tx_result_rx: Mutex<Option<crossbeam_channel::Receiver<TxResult>>>,
}

impl VaraClient {
    /// Create a new VaraClient (not connected yet)
    pub fn new(config: VaraConfig) -> Result<Self, VaraError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .max_blocking_threads(32) // bound fire-and-forget tx threads
            .build()
            .map_err(|e| VaraError::Runtime(e.to_string()))?;

        let (tx_result_tx, tx_result_rx) = crossbeam_channel::unbounded();

        Ok(Self {
            config,
            runtime,
            inner: None,
            connected: false,
            agent_locks: Arc::new(Mutex::new(HashMap::new())),
            tx_result_tx,
            tx_result_rx: Mutex::new(Some(tx_result_rx)),
        })
    }

    /// Take the transaction result receiver. Can only be called once.
    /// Give this to ExchangeAgent so it can process on-chain tx outcomes.
    pub fn take_tx_result_receiver(&self) -> Option<crossbeam_channel::Receiver<TxResult>> {
        self.tx_result_rx.lock().unwrap().take()
    }

    /// Create from environment variables
    pub fn from_env() -> Result<Self, VaraError> {
        let config = VaraConfig::from_env()?;
        Self::new(config)
    }

    /// Get or create a per-agent mutex to serialize txs from the same keypair.
    /// Normalizes agent_id so that different IDs mapping to the same keypair
    /// share a lock (prevents nonce collisions).
    fn agent_lock(&self, agent_id: u32) -> Arc<Mutex<()>> {
        let normalized = super::keystore::normalize_agent_id(agent_id);
        let mut map = self.agent_locks.lock().unwrap();
        map.entry(normalized).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
    }

    /// Connect to the Vara network
    pub fn connect(&mut self) -> Result<(), VaraError> {
        let config = self.config.clone();

        let result = self.runtime.block_on(async {
            // Parse WebSocket address
            let ws_address = WSAddress::try_new(&config.ws_endpoint, None)
                .map_err(|e| VaraError::Config(format!("Invalid WS endpoint: {}", e)))?;

            // Connect to node
            println!("[Vara] Connecting to {}...", config.ws_endpoint);
            let api = GearApi::init(ws_address).await?;

            // Get latest block
            let last_block = api.last_block_number().await?;
            println!("[Vara] Connected! Latest block: #{}", last_block);

            // Parse contract address
            let contract_id = parse_hex_bytes32(&config.contract_address)?;
            println!("[Vara] Contract: {}", config.contract_address);

            // Initialize keystore from seeds.json
            let keystore = KeystoreManager::new(&config.keystore_path, &config.passphrase_path)?;
            println!("[Vara] Keystore: {} (seeds.json)", config.keystore_path);

            Ok::<_, VaraError>(VaraClientInner {
                api,
                contract_id,
                keystore,
                gas_limits: GasLimits::from_default(config.gas_limit),
            })
        })?;

        self.inner = Some(Arc::new(RwLock::new(result)));
        self.connected = true;

        Ok(())
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Get the contract address
    pub fn contract_address(&self) -> &str {
        &self.config.contract_address
    }

    /// Get block time in milliseconds
    pub fn block_time_ms(&self) -> u64 {
        self.config.block_time_ms
    }

    /// Get a reference to the inner state, or return "Not connected" error.
    fn inner_ref(&self) -> Result<&Arc<RwLock<VaraClientInner>>, VaraError> {
        self.inner.as_ref().ok_or_else(|| VaraError::Connection("Not connected".into()))
    }

    /// Get latest block number
    pub fn latest_block(&self) -> Result<u32, VaraError> {
        let inner = self.inner_ref()?;
        self.runtime.block_on(async {
            let guard = inner.read().await;
            let block = guard.api.last_block_number().await?;
            Ok(block)
        })
    }

    /// Load keypair for an agent
    pub fn load_keypair(&self, agent_id: u32) -> Result<String, VaraError> {
        let inner = self.inner_ref()?;
        self.runtime.block_on(async {
            let guard = inner.read().await;
            let keypair = guard.keystore.load_keypair_for_agent(agent_id)?;
            Ok(keypair.address.clone())
        })
    }

    /// Preload all bot keypairs
    pub fn preload_keypairs(&self, count: u32) -> Result<usize, VaraError> {
        let inner = self.inner_ref()?;
        self.runtime.block_on(async {
            let guard = inner.read().await;
            let loaded = guard.keystore.preload_all(count)?;
            Ok(loaded)
        })
    }

    // ========== Contract Mutations ==========

    /// Extract (keypair, api, contract_id, gas_limits) from inner under a read-lock.
    /// Used by every mutation method.
    fn read_agent_context(&self, agent_id: u32) -> Result<(super::keystore::KeyPair, GearApi, [u8; 32], GasLimits), VaraError> {
        let inner = self.inner_ref()?;
        self.runtime.block_on(async {
            let guard = inner.read().await;
            let kp = guard.keystore.load_keypair_for_agent(agent_id)?.clone();
            Ok((kp, guard.api.clone(), guard.contract_id, guard.gas_limits))
        })
    }

    /// Deposit collateral (virtual balances) — single, blocking.
    /// Waits for on-chain reply to confirm the deposit succeeded.
    pub fn deposit(&self, agent_id: u32, amount: U256) -> Result<H256, VaraError> {
        let (keypair, api, contract_id, gas_limits) = self.read_agent_context(agent_id)?;
        println!("[Vara] Deposit from {} (amount={})", keypair.address, amount);

        self.runtime.block_on(async {
            let env = GclientEnv::new(api).with_suri(keypair.suri());
            let actor = Actor::<VaraPerpsProgram, GclientEnv>::new(env, contract_id.into());
            let mut service = actor.vara_perps();
            service
                .deposit(amount)
                .with_gas_limit(gas_limits.deposit)
                .await
                .map_err(|e| VaraError::Transaction(format!("Deposit rejected: {}", e)))?;
            Ok([0u8; 32])
        })
    }

    /// Deposit collateral for many agents in parallel, in batches.
    /// Each agent signs its own transaction, so there are no nonce conflicts.
    /// Sends BATCH_SIZE transactions concurrently, waits, then next batch.
    /// Returns (success_count, fail_count).
    pub fn deposit_batch(&self, deposits: &[(u32, U256)]) -> Result<(usize, usize), VaraError> {
        const BATCH_SIZE: usize = 20;
        let inner = self.inner_ref()?;

        self.runtime.block_on(async {
            // Pre-load keypairs under a read lock (all keys loaded at init)
                let tasks: Vec<_> = {
                let guard = inner.read().await;
                let mut tasks = Vec::with_capacity(deposits.len());
                for (agent_id, amount) in deposits {
                    let keypair = guard.keystore.load_keypair_for_agent(*agent_id)?.clone();
                    let contract_id = guard.contract_id;
                    let gas_limit = guard.gas_limits.deposit;
                    tasks.push((
                        keypair.address.clone(),
                        keypair.suri().to_string(),
                        guard.api.clone(),
                        contract_id,
                        gas_limit,
                        *amount,
                    ));
                }
                tasks
            };
            // Lock released — send in batches
            let total = tasks.len();
            let num_batches = (total + BATCH_SIZE - 1) / BATCH_SIZE;
            println!("[Vara] Sending {} deposits in {} batches of {}...", total, num_batches, BATCH_SIZE);

            let mut success = 0usize;
            let mut failed = 0usize;

            for (batch_idx, chunk) in tasks.chunks(BATCH_SIZE).enumerate() {
                println!(
                    "[Vara] Batch {}/{} ({} txs)...",
                    batch_idx + 1,
                    num_batches,
                    chunk.len()
                );

                let futures: Vec<_> = chunk
                    .iter()
                    .map(|(address, suri, api, contract_id, gas_limit, amount)| {
                        let address = address.clone();
                        let suri = suri.clone();
                        let api = api.clone();
                        let contract_id = *contract_id;
                        let gas_limit = *gas_limit;
                        let amount = *amount;
                        async move {
                            let env = GclientEnv::new(api).with_suri(suri);
                            let actor =
                                Actor::<VaraPerpsProgram, GclientEnv>::new(env, contract_id.into());
                            let mut service = actor.vara_perps();
                            match service
                                .deposit(amount)
                                .with_gas_limit(gas_limit)
                                .await
                            {
                                Ok(_) => {
                                    println!("[Vara] ✓ Deposit {} (amount={})", address, amount);
                                    true
                                }
                                Err(e) => {
                                    eprintln!("[Vara] ✗ Deposit {} failed: {}", address, e);
                                    false
                                }
                            }
                        }
                    })
                    .collect();

                let results = sails_rs::prelude::futures::future::join_all(futures).await;
                for ok in results {
                    if ok {
                        success += 1;
                    } else {
                        failed += 1;
                    }
                }

                // Pause between batches to avoid RPC overload
                if batch_idx + 1 < num_batches {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            }

            println!(
                "[Vara] Deposits done: {}/{} success, {} failed",
                success, total, failed
            );
            Ok((success, failed))
        })
    }

    /// Withdraw collateral
    pub fn withdraw(&self, agent_id: u32, amount: U256) -> Result<H256, VaraError> {
        let (keypair, api, contract_id, gas_limits) = self.read_agent_context(agent_id)?;
        println!("[Vara] Withdraw from {} (amount={})", keypair.address, amount);

        self.runtime.block_on(async {
            let env = GclientEnv::new(api).with_suri(keypair.suri());
            let actor = Actor::<VaraPerpsProgram, GclientEnv>::new(env, contract_id.into());
            let mut service = actor.vara_perps();
            service
                .withdraw(amount)
                .with_gas_limit(gas_limits.withdraw)
                .await
                .map_err(|e| VaraError::Transaction(format!("Withdraw rejected: {}", e)))?;
            Ok([0u8; 32])
        })
    }

    /// Add liquidity (pool funding)
    pub fn add_liquidity(&self, agent_id: u32, amount: U256) -> Result<H256, VaraError> {
        let (keypair, api, contract_id, gas_limits) = self.read_agent_context(agent_id)?;
        println!("[Vara] AddLiquidity from {} (amount={})", keypair.address, amount);

        self.runtime.block_on(async {
            let env = GclientEnv::new(api).with_suri(keypair.suri());
            let actor = Actor::<VaraPerpsProgram, GclientEnv>::new(env, contract_id.into());
            let mut service = actor.vara_perps();
            service
                .add_liquidity(amount)
                .with_gas_limit(gas_limits.add_liquidity)
                .await
                .map_err(|e| VaraError::Transaction(format!("AddLiquidity rejected: {}", e)))?;
            Ok([0u8; 32])
        })
    }

    /// Submit a limit/stop/TP order to the contract (non-blocking).
    /// Runs on the bounded blocking thread pool. Returns immediately.
    /// Actual result (OrderId or error) is sent via `tx_result_tx` channel.
    pub fn submit_order(&self, agent_id: u32, order: &Order) -> Result<OrderId, VaraError> {
        let order = order.clone();
        fire_and_forget!(self, agent_id, TxType::SubmitOrder, order, |kp, api, cid, gas, tx| {
            let detail = format!("{:?} {:?} size={} from {}", order.order_type, order.side, order.size_delta_usd, kp.address);
            println!("[Vara] SubmitOrder {}", detail);
            let env = GclientEnv::new(api).with_suri(kp.suri());
            let actor = Actor::<VaraPerpsProgram, GclientEnv>::new(env, cid.into());
            let mut service = actor.vara_perps();
            match service.submit_order(order).with_gas_limit(gas.submit_order).await {
                Ok(oid) => {
                    println!("[Vara] SubmitOrder OK -> OrderId #{}", oid.0);
                    let _ = tx.send(TxResult { agent_id, tx_type: TxType::SubmitOrder, success: true, order_id: Some(oid.0), error: None, detail });
                }
                Err(e) => {
                    eprintln!("[Vara] SubmitOrder FAILED: {}", e);
                    let _ = tx.send(TxResult { agent_id, tx_type: TxType::SubmitOrder, success: false, order_id: None, error: Some(e.to_string()), detail });
                }
            }
        });
        Ok(OrderId(0))
    }

    /// Submit order + execute in one shot — non-blocking.
    /// SubmitOrder awaits reply to get OrderId, then ExecuteOrder awaits reply.
    /// Both results are sent via `tx_result_tx` channel.
    /// Runs on the bounded blocking thread pool (sails futures are !Send).
    pub fn submit_and_execute_order_async(
        &self,
        agent_id: u32,
        order: Order,
        oracle_input: OracleInput,
    ) -> Result<(), VaraError> {
        fire_and_forget!(self, agent_id, TxType::SubmitAndExecute, order, oracle_input, |kp, api, cid, gas, tx| {
            let detail = format!("{:?} {:?} size={} from {}", order.order_type, order.side, order.size_delta_usd, kp.address);
            println!("[Vara] SubmitOrder+Execute {}", detail);

            // 1) SubmitOrder — await reply to get OrderId
            let env = GclientEnv::new(api.clone()).with_suri(kp.suri());
            let actor = Actor::<VaraPerpsProgram, GclientEnv>::new(env, cid.into());
            let mut service = actor.vara_perps();
            let order_id = match service.submit_order(order.clone()).with_gas_limit(gas.submit_order).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("[Vara] SubmitOrder FAILED: {}", e);
                    let _ = tx.send(TxResult { agent_id, tx_type: TxType::SubmitAndExecute, success: false, order_id: None, error: Some(e.to_string()), detail });
                    return;
                }
            };
            let oid = order_id.0;
            println!("[Vara] Got OrderId #{}, executing...", oid);

            // 2) ExecuteOrder — await reply for confirmation
            let env2 = GclientEnv::new(api).with_suri(kp.suri());
            let actor2 = Actor::<VaraPerpsProgram, GclientEnv>::new(env2, cid.into());
            let mut service2 = actor2.vara_perps();
            match service2.execute_order(order_id, oracle_input).with_gas_limit(gas.execute_order).await {
                Ok(_) => {
                    println!("[Vara] ExecuteOrder #{} OK", oid);
                    let _ = tx.send(TxResult { agent_id, tx_type: TxType::SubmitAndExecute, success: true, order_id: Some(oid), error: None, detail });
                }
                Err(e) => {
                    eprintln!("[Vara] ExecuteOrder #{} FAILED: {}", oid, e);
                    let _ = tx.send(TxResult { agent_id, tx_type: TxType::SubmitAndExecute, success: false, order_id: Some(oid), error: Some(e.to_string()), detail });
                }
            }
        });
        Ok(())
    }

    /// Cancel an order (non-blocking, result via channel)
    pub fn cancel_order(&self, agent_id: u32, order_id: OrderId) -> Result<H256, VaraError> {
        fire_and_forget!(self, agent_id, TxType::CancelOrder, order_id, |kp, api, cid, gas, tx| {
            let oid = order_id.0;
            let detail = format!("#{} from {}", oid, kp.address);
            println!("[Vara] CancelOrder {}", detail);
            let env = GclientEnv::new(api).with_suri(kp.suri());
            let actor = Actor::<VaraPerpsProgram, GclientEnv>::new(env, cid.into());
            let mut service = actor.vara_perps();
            match service.cancel_order(order_id).with_gas_limit(gas.cancel_order).await {
                Ok(_) => {
                    println!("[Vara] CancelOrder #{} OK", oid);
                    let _ = tx.send(TxResult { agent_id, tx_type: TxType::CancelOrder, success: true, order_id: Some(oid), error: None, detail });
                }
                Err(e) => {
                    eprintln!("[Vara] CancelOrder #{} FAILED: {}", oid, e);
                    let _ = tx.send(TxResult { agent_id, tx_type: TxType::CancelOrder, success: false, order_id: Some(oid), error: Some(e.to_string()), detail });
                }
            }
        });
        Ok([0u8; 32])
    }

    /// Execute a pending order — keeper action (non-blocking, result via channel)
    pub fn execute_order(&self, agent_id: u32, order_id: OrderId, oracle_input: &OracleInput) -> Result<H256, VaraError> {
        let oracle_input = oracle_input.clone();
        fire_and_forget!(self, agent_id, TxType::ExecuteOrder, order_id, oracle_input, |kp, api, cid, gas, tx| {
            let oid = order_id.0;
            let detail = format!("#{} by keeper {}", oid, kp.address);
            println!("[Vara] ExecuteOrder {}", detail);
            let env = GclientEnv::new(api).with_suri(kp.suri());
            let actor = Actor::<VaraPerpsProgram, GclientEnv>::new(env, cid.into());
            let mut service = actor.vara_perps();
            match service.execute_order(order_id, oracle_input).with_gas_limit(gas.execute_order).await {
                Ok(_) => {
                    println!("[Vara] ExecuteOrder #{} OK", oid);
                    let _ = tx.send(TxResult { agent_id, tx_type: TxType::ExecuteOrder, success: true, order_id: Some(oid), error: None, detail });
                }
                Err(e) => {
                    eprintln!("[Vara] ExecuteOrder #{} FAILED: {}", oid, e);
                    let _ = tx.send(TxResult { agent_id, tx_type: TxType::ExecuteOrder, success: false, order_id: Some(oid), error: Some(e.to_string()), detail });
                }
            }
        });
        Ok([0u8; 32])
    }

    // ========== Contract Queries ==========

    /// Shorthand for query errors.
    fn query_err(e: impl std::fmt::Display) -> VaraError {
        VaraError::Query(format!("Failed to read state: {}", e))
    }

    /// Get an order by ID
    pub fn get_order(&self, order_id: OrderId) -> Result<Option<Order>, VaraError> {
        query!(self, |s| s.get_order(order_id).query().await.map_err(Self::query_err))
    }

    /// Get a position by key
    pub fn get_position(&self, key: &PositionKey) -> Result<Option<Position>, VaraError> {
        let key = key.clone();
        query!(self, |s| s.get_position(key).query().await.map_err(Self::query_err))
    }

    /// Get all positions (for liquidators)
    pub fn get_all_positions(&self) -> Result<Vec<Position>, VaraError> {
        query!(self, |s| s.get_all_positions().query().await.map_err(Self::query_err))
    }

    /// Get all pending orders (for keepers)
    pub fn get_pending_orders(&self) -> Result<Vec<Order>, VaraError> {
        query!(self, |s| s.get_pending_orders().query().await.map_err(Self::query_err))
    }

    /// Get balance for account
    pub fn get_balance(&self, account: ActorId) -> Result<U256, VaraError> {
        query!(self, |s| s.balance_of(account).query().await.map_err(Self::query_err))
    }

    /// Get claimable amount for an account
    pub fn get_claimable(&self, account: ActorId) -> Result<U256, VaraError> {
        query!(self, |s| s.get_claimable(account).query().await.map_err(Self::query_err))
    }

    /// Calculate liquidation price for a position
    pub fn calculate_liquidation_price(
        &self,
        key: &PositionKey,
        oracle_input: &OracleInput,
    ) -> Result<U256, VaraError> {
        let key = key.clone();
        let oi = oracle_input.clone();
        query!(self, |s| s.calculate_liquidation_price(key, oi).query().await.map_err(Self::query_err))
    }

    /// Check if a position is liquidatable
    pub fn is_liquidatable(&self, key: &PositionKey, oracle_input: &OracleInput) -> Result<LiquidationPreview, VaraError> {
        let key = key.clone();
        let oi = oracle_input.clone();
        query!(self, |s| s.is_liquidatable_by_margin(key, oi).query().await.map_err(Self::query_err))
    }

    // ========== Async Sync ==========

    /// Non-blocking: fetch all positions on the blocking pool, compute OI aggregates,
    /// and send (oi_long_micro, oi_short_micro) via the provided channel.
    /// This unblocks the kernel thread that was previously stalled for 500ms-2s
    /// while waiting for the RPC response.
    pub fn fetch_oi_async(&self, sender: crossbeam_channel::Sender<(i128, i128)>) {
        let inner = match self.inner_ref() {
            Ok(i) => i.clone(),
            Err(e) => {
                eprintln!("[Vara] fetch_oi_async: {}", e);
                return;
            }
        };

        self.runtime.handle().spawn_blocking(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[Vara] fetch_oi_async: runtime error: {}", e);
                    return;
                }
            };
            rt.block_on(async move {
                let guard = inner.read().await;
                let env = GclientEnv::new(guard.api.clone());
                let actor =
                    Actor::<VaraPerpsProgram, GclientEnv>::new(env, guard.contract_id.into());
                let s = actor.vara_perps();
                match s.get_all_positions().query().await {
                    Ok(positions) => {
                        let divisor = U256::exp10(24);
                        let mut oi_long: i128 = 0;
                        let mut oi_short: i128 = 0;
                        for p in positions.iter() {
                            if p.size_usd.is_zero() {
                                continue;
                            }
                            let size_micro = (U256::from(p.size_usd) / divisor).low_u64() as i128;
                            match p.key.side {
                                VaraSide::Long => oi_long += size_micro,
                                VaraSide::Short => oi_short += size_micro,
                            }
                        }
                        let _ = sender.send((oi_long, oi_short));
                    }
                    Err(e) => {
                        eprintln!("[Vara] fetch_oi_async: query failed: {}", e);
                    }
                }
            });
        });
    }

    // ========== Utility Methods ==========

    /// Get address string for an agent
    pub fn get_address(&self, agent_id: u32) -> Result<String, VaraError> {
        let inner = self.inner_ref()?;
        self.runtime.block_on(async {
            let guard = inner.read().await;
            let normalized = super::keystore::normalize_agent_id(agent_id);
            let name = format!("bot_{:03}", normalized);
            let keypair = guard
                .keystore
                .get_keypair(&name)
                .ok_or_else(|| VaraError::Keystore(KeystoreError::NotFound(name)))?;
            Ok(keypair.address.clone())
        })
    }

    /// Get on-chain ActorId for an agent (decoded from SS58 address)
    pub fn get_actor_id(&self, agent_id: u32) -> Result<ActorId, VaraError> {
        let address = self.get_address(agent_id)?;
        actor_id_from_address(&address)
    }
}

/// Parse hex address to 32-byte array
fn parse_hex_bytes32(hex_str: &str) -> Result<[u8; 32], VaraError> {
    let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);

    if hex_str.len() != 64 {
        return Err(VaraError::Config(format!(
            "Invalid address length: expected 64 hex chars, got {}",
            hex_str.len()
        )));
    }

    let bytes = hex::decode(hex_str).map_err(|e| VaraError::Config(format!("Invalid hex: {}", e)))?;

    let mut result = [0u8; 32];
    result.copy_from_slice(&bytes);
    Ok(result)
}

fn actor_id_from_address(address: &str) -> Result<ActorId, VaraError> {
    let account = AccountId32::from_ss58check(address)
        .map_err(|e| VaraError::Config(format!("Invalid SS58 address {}: {}", address, e)))?;
    let bytes: [u8; 32] = account.into();
    Ok(bytes.into())
}

impl std::fmt::Debug for VaraClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaraClient")
            .field("endpoint", &self.config.ws_endpoint)
            .field("contract", &self.config.contract_address)
            .field("connected", &self.connected)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_bytes32() {
        let addr = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let result = parse_hex_bytes32(addr).unwrap();
        assert_eq!(result[31], 1);
        assert_eq!(result[0], 0);
    }

    #[test]
    fn test_parse_hex_bytes32_no_prefix() {
        let addr = "0000000000000000000000000000000000000000000000000000000000000002";
        let result = parse_hex_bytes32(addr).unwrap();
        assert_eq!(result[31], 2);
    }

    #[test]
    fn test_parse_hex_bytes32_invalid() {
        let addr = "0x123"; // Too short
        assert!(parse_hex_bytes32(addr).is_err());
    }
}
