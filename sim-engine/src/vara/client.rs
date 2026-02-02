use std::sync::Arc;

use gclient::{GearApi, WSAddress};
use hex;
use primitive_types::U256;
use tokio::runtime::Runtime;
use tokio::sync::RwLock;

/// 32-byte hash type (message ID, block hash, etc.)
pub type H256 = [u8; 32];

use super::codec::VaraPerpsCodec;
use super::keystore::{KeyPair, KeystoreError, KeystoreManager};
use super::types::{ActorId, LiquidationPreview, OraclePrices, Order, OrderId, PendingOrder, Position, PositionKey};

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
    /// Encoding error
    Encoding(String),
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
            Self::Encoding(s) => write!(f, "Encoding error: {}", s),
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

        let keystore_path = std::env::var("VARA_KEYSTORE_PATH")
            .unwrap_or_else(|_| "keys/Library/Application Support/gring".to_string());

        let passphrase_path = std::env::var("VARA_PASSPHRASE_FILE").unwrap_or_else(|_| "keys/.passphrase".to_string());

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

/// Shared state for VaraClient
struct VaraClientInner {
    /// GearApi connection
    api: GearApi,
    /// Contract program ID as bytes
    contract_id: [u8; 32],
    /// Keystore manager
    keystore: KeystoreManager,
    /// Gas limit for transactions
    gas_limit: u64,
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
}

impl VaraClient {
    /// Create a new VaraClient (not connected yet)
    pub fn new(config: VaraConfig) -> Result<Self, VaraError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| VaraError::Runtime(e.to_string()))?;

        Ok(Self {
            config,
            runtime,
            inner: None,
            connected: false,
        })
    }

    /// Create from environment variables
    pub fn from_env() -> Result<Self, VaraError> {
        let config = VaraConfig::from_env()?;
        Self::new(config)
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

            // Initialize keystore
            let keystore = KeystoreManager::new(&config.keystore_path, &config.passphrase_path)?;
            println!("[Vara] Keystore: {}", config.keystore_path);

            Ok::<_, VaraError>(VaraClientInner {
                api,
                contract_id,
                keystore,
                gas_limit: config.gas_limit,
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

    /// Get latest block number
    pub fn latest_block(&self) -> Result<u32, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;
            let block = guard.api.last_block_number().await?;
            Ok(block)
        })
    }

    /// Load keypair for an agent
    pub fn load_keypair(&self, agent_id: u32) -> Result<String, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let mut guard = inner.write().await;
            let keypair = guard.keystore.load_keypair_for_agent(agent_id)?;
            Ok(keypair.address.clone())
        })
    }

    /// Preload all bot keypairs
    pub fn preload_keypairs(&self, count: u32) -> Result<usize, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let mut guard = inner.write().await;
            let loaded = guard.keystore.preload_all(count)?;
            Ok(loaded)
        })
    }

    // ========== Contract Mutations ==========

    /// Submit an order to the contract
    pub fn submit_order(&self, agent_id: u32, order: &Order) -> Result<H256, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            // Get keypair for signing
            let keypair = guard
                .keystore
                .get_keypair(&format!("bot_{:03}", agent_id))
                .ok_or_else(|| VaraError::Keystore(KeystoreError::NotFound(format!("bot_{:03}", agent_id))))?;

            println!(
                "[Vara] SubmitOrder from {} ({:?} {:?} size={})",
                keypair.address, order.order_type, order.side, order.size_delta_usd
            );

            // Encode the message using Sails codec
            let payload = VaraPerpsCodec::submit_order(order);

            // Send message to contract
            // Note: In production, we need to sign with the keypair
            // For now, using the default account from GearApi
            let signed_api = api_with_keypair(&guard.api, keypair)?;

            let _message_id = signed_api
                .send_message_bytes(guard.contract_id.into(), payload, guard.gas_limit, 0)
                .await
                .map_err(|e| VaraError::Transaction(format!("Failed to send message: {}", e)))?;

            Ok([0u8; 32])
        })
    }

    /// Cancel an order
    pub fn cancel_order(&self, agent_id: u32, order_id: OrderId) -> Result<H256, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let keypair = guard
                .keystore
                .get_keypair(&format!("bot_{:03}", agent_id))
                .ok_or_else(|| VaraError::Keystore(KeystoreError::NotFound(format!("bot_{:03}", agent_id))))?;

            println!("[Vara] CancelOrder #{} from {}", order_id.0, keypair.address);

            let payload = VaraPerpsCodec::cancel_order(order_id);

            let signed_api = api_with_keypair(&guard.api, keypair)?;

            let _message_id = signed_api
                .send_message_bytes(guard.contract_id.into(), payload, guard.gas_limit, 0)
                .await
                .map_err(|e| VaraError::Transaction(format!("Failed to send message: {}", e)))?;

            Ok([0u8; 32])
        })
    }

    /// Execute a pending order (keeper action)
    pub fn execute_order(&self, agent_id: u32, order_id: OrderId, prices: &OraclePrices) -> Result<H256, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let keypair = guard
                .keystore
                .get_keypair(&format!("bot_{:03}", agent_id))
                .ok_or_else(|| VaraError::Keystore(KeystoreError::NotFound(format!("bot_{:03}", agent_id))))?;

            println!(
                "[Vara] ExecuteOrder #{} by keeper {} (price={})",
                order_id.0,
                keypair.address,
                prices.index_mid()
            );

            let payload = VaraPerpsCodec::execute_order(order_id, prices);

            let signed_api = api_with_keypair(&guard.api, keypair)?;

            let _message_id = signed_api
                .send_message_bytes(guard.contract_id.into(), payload, guard.gas_limit, 0)
                .await
                .map_err(|e| VaraError::Transaction(format!("Failed to send message: {}", e)))?;

            Ok([0u8; 32])
        })
    }

    // ========== Contract Queries ==========

    /// Get an order by ID
    pub fn get_order(&self, order_id: OrderId) -> Result<Option<Order>, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let payload = VaraPerpsCodec::get_order(order_id);

            let state = guard
                .api
                .read_state_bytes(guard.contract_id.into(), payload)
                .await
                .map_err(|e| VaraError::Query(format!("Failed to read state: {}", e)))?;

            VaraPerpsCodec::decode_order_response(&state).map_err(|e| VaraError::Encoding(e))
        })
    }

    /// Get a position by key
    pub fn get_position(&self, key: &PositionKey) -> Result<Option<Position>, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let payload = VaraPerpsCodec::get_position(key);

            let state = guard
                .api
                .read_state_bytes(guard.contract_id.into(), payload)
                .await
                .map_err(|e| VaraError::Query(format!("Failed to read state: {}", e)))?;

            VaraPerpsCodec::decode_position_response(&state).map_err(|e| VaraError::Encoding(e))
        })
    }

    /// Get all positions (for liquidators)
    pub fn get_all_positions(&self) -> Result<Vec<Position>, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let payload = VaraPerpsCodec::get_all_positions();

            let state = guard
                .api
                .read_state_bytes(guard.contract_id.into(), payload)
                .await
                .map_err(|e| VaraError::Query(format!("Failed to read state: {}", e)))?;

            VaraPerpsCodec::decode_all_positions_response(&state).map_err(|e| VaraError::Encoding(e))
        })
    }

    /// Get all pending orders (for keepers)
    pub fn get_pending_orders(&self) -> Result<Vec<PendingOrder>, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let payload = VaraPerpsCodec::get_pending_orders();

            let state = guard
                .api
                .read_state_bytes(guard.contract_id.into(), payload)
                .await
                .map_err(|e| VaraError::Query(format!("Failed to read state: {}", e)))?;

            VaraPerpsCodec::decode_pending_orders_response(&state).map_err(|e| VaraError::Encoding(e))
        })
    }

    /// Get claimable amount for an account
    pub fn get_claimable(&self, account: ActorId) -> Result<U256, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let payload = VaraPerpsCodec::get_claimable(account);

            let state = guard
                .api
                .read_state_bytes(guard.contract_id.into(), payload)
                .await
                .map_err(|e| VaraError::Query(format!("Failed to read state: {}", e)))?;

            VaraPerpsCodec::decode_claimable_response(&state).map_err(|e| VaraError::Encoding(e))
        })
    }

    /// Calculate liquidation price for a position
    pub fn calculate_liquidation_price(&self, key: &PositionKey) -> Result<Option<U256>, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let payload = VaraPerpsCodec::calculate_liquidation_price(key);

            let state = guard
                .api
                .read_state_bytes(guard.contract_id.into(), payload)
                .await
                .map_err(|e| VaraError::Query(format!("Failed to read state: {}", e)))?;

            VaraPerpsCodec::decode_liquidation_price_response(&state).map_err(|e| VaraError::Encoding(e))
        })
    }

    /// Check if a position is liquidatable
    pub fn is_liquidatable(&self, key: &PositionKey) -> Result<Option<LiquidationPreview>, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let payload = VaraPerpsCodec::is_liquidatable_by_margin(key);

            let state = guard
                .api
                .read_state_bytes(guard.contract_id.into(), payload)
                .await
                .map_err(|e| VaraError::Query(format!("Failed to read state: {}", e)))?;

            VaraPerpsCodec::decode_liquidation_preview_response(&state).map_err(|e| VaraError::Encoding(e))
        })
    }

    // ========== Utility Methods ==========

    /// Get the account ID for an agent
    pub fn get_account_id(&self, agent_id: u32) -> Result<ActorId, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let keypair = guard
                .keystore
                .get_keypair(&format!("bot_{:03}", agent_id))
                .ok_or_else(|| VaraError::Keystore(KeystoreError::NotFound(format!("bot_{:03}", agent_id))))?;

            Ok(keypair.account_id())
        })
    }

    /// Get address string for an agent
    pub fn get_address(&self, agent_id: u32) -> Result<String, VaraError> {
        let inner = self
            .inner
            .as_ref()
            .ok_or_else(|| VaraError::Connection("Not connected".to_string()))?;

        self.runtime.block_on(async {
            let guard = inner.read().await;

            let keypair = guard
                .keystore
                .get_keypair(&format!("bot_{:03}", agent_id))
                .ok_or_else(|| VaraError::Keystore(KeystoreError::NotFound(format!("bot_{:03}", agent_id))))?;

            Ok(keypair.address.clone())
        })
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

fn api_with_keypair(api: &GearApi, keypair: &KeyPair) -> Result<GearApi, VaraError> {
    let suri = format!("0x{}", hex::encode(keypair.seed_phrase()));
    api.clone()
        .with(&suri)
        .map_err(|e| VaraError::Transaction(format!("Signer error: {}", e)))
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
