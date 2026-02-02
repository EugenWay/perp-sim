//! Sails codec for VaraPerps contract
//!
//! Handles encoding/decoding of messages according to Sails conventions.
//! Service: VaraPerps
//!
//! Message format:
//! - Service route (computed from service name via blake2)
//! - Method route (computed from method name via blake2)  
//! - SCALE-encoded arguments

use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest};
use parity_scale_codec::{Decode, Encode};

use super::types::*;

/// Service name for route computation
const SERVICE_NAME: &str = "VaraPerps";

/// Blake2b-256 hasher type
type Blake2b256 = Blake2b<U32>;

/// Compute Sails route from a name (first 4 bytes of blake2b-256 hash)
fn compute_route(name: &str) -> [u8; 4] {
    let mut hasher = Blake2b256::new();
    hasher.update(name.as_bytes());
    let result = hasher.finalize();
    let mut route = [0u8; 4];
    route.copy_from_slice(&result[..4]);
    route
}

/// Get the service route for VaraPerps
pub fn service_route() -> [u8; 4] {
    compute_route(SERVICE_NAME)
}

/// Message builder for VaraPerps service
pub struct VaraPerpsCodec;

impl VaraPerpsCodec {
    // ========== Mutations ==========

    /// Encode SubmitOrder message
    pub fn submit_order(order: &Order) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&service_route());
        payload.extend_from_slice(&compute_route("SubmitOrder"));
        payload.extend_from_slice(&order.encode());
        payload
    }

    /// Encode CancelOrder message
    pub fn cancel_order(order_id: OrderId) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&service_route());
        payload.extend_from_slice(&compute_route("CancelOrder"));
        payload.extend_from_slice(&order_id.encode());
        payload
    }

    /// Encode ExecuteOrder message (with oracle prices)
    pub fn execute_order(order_id: OrderId, prices: &OraclePrices) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&service_route());
        payload.extend_from_slice(&compute_route("ExecuteOrder"));
        payload.extend_from_slice(&order_id.encode());
        payload.extend_from_slice(&prices.encode());
        payload
    }

    // ========== Queries ==========

    /// Encode GetOrder query
    pub fn get_order(order_id: OrderId) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&service_route());
        payload.extend_from_slice(&compute_route("GetOrder"));
        payload.extend_from_slice(&order_id.encode());
        payload
    }

    /// Encode GetPosition query
    pub fn get_position(key: &PositionKey) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&service_route());
        payload.extend_from_slice(&compute_route("GetPosition"));
        payload.extend_from_slice(&key.encode());
        payload
    }

    /// Encode GetAllPositions query
    pub fn get_all_positions() -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&service_route());
        payload.extend_from_slice(&compute_route("GetAllPositions"));
        // No arguments
        payload
    }

    /// Encode GetPendingOrders query
    pub fn get_pending_orders() -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&service_route());
        payload.extend_from_slice(&compute_route("GetPendingOrders"));
        // No arguments
        payload
    }

    /// Encode GetClaimable query
    pub fn get_claimable(account: ActorId) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&service_route());
        payload.extend_from_slice(&compute_route("GetClaimable"));
        payload.extend_from_slice(&account.encode());
        payload
    }

    /// Encode CalculateLiquidationPrice query
    pub fn calculate_liquidation_price(key: &PositionKey) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&service_route());
        payload.extend_from_slice(&compute_route("CalculateLiquidationPrice"));
        payload.extend_from_slice(&key.encode());
        payload
    }

    /// Encode IsLiquidatableByMargin query
    pub fn is_liquidatable_by_margin(key: &PositionKey) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&service_route());
        payload.extend_from_slice(&compute_route("IsLiquidatableByMargin"));
        payload.extend_from_slice(&key.encode());
        payload
    }

    // ========== Response Decoders ==========

    /// Decode response, skipping service/method routes (first 8 bytes)
    fn decode_response<T: Decode>(data: &[u8]) -> Result<T, String> {
        if data.len() < 8 {
            return Err(format!("Response too short: {} bytes", data.len()));
        }
        // Skip service route (4 bytes) + method route (4 bytes)
        let payload = &data[8..];
        T::decode(&mut &payload[..]).map_err(|e| format!("Decode error: {}", e))
    }

    /// Decode GetOrder response
    pub fn decode_order_response(data: &[u8]) -> Result<Option<Order>, String> {
        Self::decode_response(data)
    }

    /// Decode GetPosition response
    pub fn decode_position_response(data: &[u8]) -> Result<Option<Position>, String> {
        Self::decode_response(data)
    }

    /// Decode GetAllPositions response
    pub fn decode_all_positions_response(data: &[u8]) -> Result<Vec<Position>, String> {
        Self::decode_response(data)
    }

    /// Decode GetPendingOrders response
    pub fn decode_pending_orders_response(data: &[u8]) -> Result<Vec<PendingOrder>, String> {
        Self::decode_response(data)
    }

    /// Decode GetClaimable response
    pub fn decode_claimable_response(data: &[u8]) -> Result<primitive_types::U256, String> {
        Self::decode_response(data)
    }

    /// Decode CalculateLiquidationPrice response
    pub fn decode_liquidation_price_response(data: &[u8]) -> Result<Option<primitive_types::U256>, String> {
        Self::decode_response(data)
    }

    /// Decode IsLiquidatableByMargin response
    pub fn decode_liquidation_preview_response(data: &[u8]) -> Result<Option<LiquidationPreview>, String> {
        Self::decode_response(data)
    }
}

/// Helper to convert micro-USD to USD(1e30) for contract
pub fn micro_usd_to_contract(micro: u64, index_decimals: u32) -> primitive_types::U256 {
    // micro-USD (1e6 = $1) to USD(1e30) per atom
    // price_per_atom = micro_usd * 10^(24 - index_decimals)
    let exp = 24u32.saturating_sub(index_decimals);
    primitive_types::U256::from(micro) * primitive_types::U256::exp10(exp as usize)
}

/// Helper to convert USD(1e30) from contract to micro-USD
pub fn contract_to_micro_usd(usd_1e30: primitive_types::U256, index_decimals: u32) -> u64 {
    let exp = 24u32.saturating_sub(index_decimals);
    let divisor = primitive_types::U256::exp10(exp as usize);
    if divisor.is_zero() {
        return 0;
    }
    (usd_1e30 / divisor).low_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_route() {
        let route = service_route();
        // Route should be 4 bytes
        assert_eq!(route.len(), 4);
        // Should be deterministic
        assert_eq!(route, service_route());
        println!("VaraPerps service route: 0x{}", hex::encode(route));
    }

    #[test]
    fn test_method_routes() {
        println!("SubmitOrder route: 0x{}", hex::encode(compute_route("SubmitOrder")));
        println!("CancelOrder route: 0x{}", hex::encode(compute_route("CancelOrder")));
        println!("ExecuteOrder route: 0x{}", hex::encode(compute_route("ExecuteOrder")));
        println!("GetOrder route: 0x{}", hex::encode(compute_route("GetOrder")));
        println!("GetPosition route: 0x{}", hex::encode(compute_route("GetPosition")));
        println!(
            "GetAllPositions route: 0x{}",
            hex::encode(compute_route("GetAllPositions"))
        );
        println!(
            "GetPendingOrders route: 0x{}",
            hex::encode(compute_route("GetPendingOrders"))
        );
    }

    #[test]
    fn test_submit_order_encoding() {
        let order = Order::market_increase(
            [1u8; 32],
            Side::Long,
            primitive_types::U256::from(1000),
            primitive_types::U256::from(5000),
            5,
            None,
            12345,
        );

        let encoded = VaraPerpsCodec::submit_order(&order);

        // Should start with service route + method route (8 bytes)
        assert!(encoded.len() > 8);

        // First 4 bytes: service route
        let service = &encoded[0..4];
        assert_eq!(service, &service_route());

        // Next 4 bytes: method route
        let method = &encoded[4..8];
        assert_eq!(method, &compute_route("SubmitOrder"));
    }

    #[test]
    fn test_price_conversion() {
        // $3000 in micro-USD for ETH (18 decimals)
        let micro = 3000_000_000u64; // $3000
        let contract_price = micro_usd_to_contract(micro, 18);

        // Convert back
        let back = contract_to_micro_usd(contract_price, 18);
        assert_eq!(back, micro);
    }
}
