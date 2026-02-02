//! VaraPerps contract types (SCALE-encoded for Sails)

use parity_scale_codec::{Decode, Encode};
use primitive_types::U256;
use scale_info::TypeInfo;

/// 32-byte account ID (same as gear_core::ids::ActorId internally)
pub type ActorId = [u8; 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encode, Decode, TypeInfo)]
pub struct OrderId(pub u64);

impl From<u64> for OrderId {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encode, Decode, TypeInfo)]
pub enum Side {
    Long,
    Short,
}

impl Side {
    pub fn opposite(&self) -> Self {
        match self {
            Side::Long => Side::Short,
            Side::Short => Side::Long,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, TypeInfo)]
pub enum OrderType {
    Increase,
    Decrease,
    Liquidation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, TypeInfo)]
pub enum ExecutionType {
    Market,
    Limit,
    StopLoss,
    TakeProfit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Encode, Decode, TypeInfo)]
pub struct SignedU256 {
    pub is_negative: bool,
    pub mag: U256,
}

impl SignedU256 {
    pub fn zero() -> Self {
        Self {
            is_negative: false,
            mag: U256::zero(),
        }
    }

    pub fn positive(mag: U256) -> Self {
        Self {
            is_negative: false,
            mag,
        }
    }

    pub fn negative(mag: U256) -> Self {
        Self { is_negative: true, mag }
    }

    pub fn to_i128(&self) -> i128 {
        let val = self.mag.low_u128() as i128;
        if self.is_negative {
            -val
        } else {
            val
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encode, Decode, TypeInfo)]
pub struct PositionKey {
    pub account: ActorId,
    pub side: Side,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, TypeInfo)]
pub struct OraclePrices {
    pub index_price_min: U256,
    pub index_price_max: U256,
    pub collateral_price_min: U256,
    pub collateral_price_max: U256,
}

impl OraclePrices {
    pub fn from_single(index_price: U256, collateral_price: U256) -> Self {
        Self {
            index_price_min: index_price,
            index_price_max: index_price,
            collateral_price_min: collateral_price,
            collateral_price_max: collateral_price,
        }
    }

    pub fn index_mid(&self) -> U256 {
        (self.index_price_min + self.index_price_max) / 2
    }

    pub fn collateral_mid(&self) -> U256 {
        (self.collateral_price_min + self.collateral_price_max) / 2
    }
}

/// Order (sizes in 1e30 USD scale)
#[derive(Debug, Clone, Encode, Decode, TypeInfo)]
pub struct Order {
    pub account: ActorId,
    pub side: Side,
    pub order_type: OrderType,
    pub execution_type: ExecutionType,
    pub collateral_delta_tokens: U256,
    pub size_delta_usd: U256,
    pub trigger_price: Option<U256>,
    pub acceptable_price: Option<U256>,
    pub withdraw_collateral_amount: U256,
    pub target_leverage_x: u32,
    pub created_at: u64,
    pub valid_from: u64,
    /// Timestamp until which order is valid
    pub valid_until: u64,
}

#[derive(Debug, Clone, Encode, Decode, TypeInfo)]
pub struct PendingOrder {
    pub order_id: OrderId,
    pub order: Order,
}

impl Order {
    /// Create a new market order to open/increase a position
    pub fn market_increase(
        account: ActorId,
        side: Side,
        collateral: U256,
        size_usd: U256,
        leverage: u32,
        acceptable_price: Option<U256>,
        now: u64,
    ) -> Self {
        Self {
            account,
            side,
            order_type: OrderType::Increase,
            execution_type: ExecutionType::Market,
            collateral_delta_tokens: collateral,
            size_delta_usd: size_usd,
            trigger_price: None,
            acceptable_price,
            withdraw_collateral_amount: U256::zero(),
            target_leverage_x: leverage,
            created_at: now,
            valid_from: now,
            valid_until: now + 3600, // 1 hour validity
        }
    }

    /// Create a new market order to close/decrease a position
    pub fn market_decrease(
        account: ActorId,
        side: Side,
        size_delta_usd: U256,
        withdraw_collateral: U256,
        acceptable_price: Option<U256>,
        now: u64,
    ) -> Self {
        Self {
            account,
            side,
            order_type: OrderType::Decrease,
            execution_type: ExecutionType::Market,
            collateral_delta_tokens: U256::zero(),
            size_delta_usd,
            trigger_price: None,
            acceptable_price,
            withdraw_collateral_amount: withdraw_collateral,
            target_leverage_x: 0,
            created_at: now,
            valid_from: now,
            valid_until: now + 3600,
        }
    }

    /// Create a limit order
    pub fn limit_order(
        account: ActorId,
        side: Side,
        order_type: OrderType,
        collateral: U256,
        size_usd: U256,
        trigger_price: U256,
        leverage: u32,
        now: u64,
        valid_until: u64,
    ) -> Self {
        Self {
            account,
            side,
            order_type,
            execution_type: ExecutionType::Limit,
            collateral_delta_tokens: collateral,
            size_delta_usd: size_usd,
            trigger_price: Some(trigger_price),
            acceptable_price: None,
            withdraw_collateral_amount: U256::zero(),
            target_leverage_x: leverage,
            created_at: now,
            valid_from: now,
            valid_until,
        }
    }

    /// Create a stop-loss order
    pub fn stop_loss(
        account: ActorId,
        side: Side,
        size_delta_usd: U256,
        trigger_price: U256,
        now: u64,
        valid_until: u64,
    ) -> Self {
        Self {
            account,
            side,
            order_type: OrderType::Decrease,
            execution_type: ExecutionType::StopLoss,
            collateral_delta_tokens: U256::zero(),
            size_delta_usd,
            trigger_price: Some(trigger_price),
            acceptable_price: None,
            withdraw_collateral_amount: U256::zero(),
            target_leverage_x: 0,
            created_at: now,
            valid_from: now,
            valid_until,
        }
    }

    /// Create a take-profit order
    pub fn take_profit(
        account: ActorId,
        side: Side,
        size_delta_usd: U256,
        trigger_price: U256,
        now: u64,
        valid_until: u64,
    ) -> Self {
        Self {
            account,
            side,
            order_type: OrderType::Decrease,
            execution_type: ExecutionType::TakeProfit,
            collateral_delta_tokens: U256::zero(),
            size_delta_usd,
            trigger_price: Some(trigger_price),
            acceptable_price: None,
            withdraw_collateral_amount: U256::zero(),
            target_leverage_x: 0,
            created_at: now,
            valid_from: now,
            valid_until,
        }
    }
}

/// Position state from the contract
#[derive(Debug, Clone, Encode, Decode, TypeInfo)]
pub struct Position {
    pub key: PositionKey,
    /// Position size in USD (scaled by 1e30)
    pub size_usd: U256,
    /// Position size in tokens
    pub size_tokens: U256,
    /// Collateral amount in tokens
    pub collateral_amount: U256,
    /// Pending price impact (positive = profit, negative = loss)
    pub pending_impact_tokens: SignedU256,
    /// Funding index at last update
    pub funding_index: SignedU256,
    /// Borrowing index at last update
    pub borrowing_index: U256,
    /// Timestamp when position was opened
    pub opened_at: u64,
    /// Timestamp of last update
    pub last_updated_at: u64,
}

impl Position {
    /// Check if position is empty (closed)
    pub fn is_empty(&self) -> bool {
        self.size_usd.is_zero()
    }

    /// Calculate leverage: size_usd / collateral_usd
    /// Returns 0 if collateral is zero
    pub fn leverage(&self, collateral_price: U256) -> u32 {
        if self.collateral_amount.is_zero() || collateral_price.is_zero() {
            return 0;
        }
        let collateral_usd = self.collateral_amount * collateral_price;
        if collateral_usd.is_zero() {
            return 0;
        }
        (self.size_usd / collateral_usd).low_u32().max(1)
    }
}

/// Liquidation preview returned by IsLiquidatableByMargin query
#[derive(Debug, Clone, Encode, Decode, TypeInfo)]
pub struct LiquidationPreview {
    /// Collateral value in USD
    pub collateral_value_usd: U256,
    /// Unrealized PnL
    pub pnl_usd: SignedU256,
    /// Price impact
    pub price_impact_usd: SignedU256,
    /// Accumulated borrowing fee
    pub borrowing_fee_usd: U256,
    /// Accumulated funding fee (can be positive or negative)
    pub funding_fee_usd: SignedU256,
    /// Fees to close the position
    pub close_fees_usd: U256,
    /// Net equity (collateral + pnl - fees)
    pub equity_usd: SignedU256,
    /// Required margin to avoid liquidation
    pub required_usd: U256,
    /// Whether position is liquidatable
    pub is_liquidatable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_side_opposite() {
        assert_eq!(Side::Long.opposite(), Side::Short);
        assert_eq!(Side::Short.opposite(), Side::Long);
    }

    #[test]
    fn test_signed_u256() {
        let pos = SignedU256::positive(U256::from(100));
        assert_eq!(pos.to_i128(), 100);

        let neg = SignedU256::negative(U256::from(50));
        assert_eq!(neg.to_i128(), -50);

        let zero = SignedU256::zero();
        assert_eq!(zero.to_i128(), 0);
    }

    #[test]
    fn test_oracle_prices() {
        let prices = OraclePrices::from_single(U256::from(3000), U256::from(1));
        assert_eq!(prices.index_mid(), U256::from(3000));
        assert_eq!(prices.collateral_mid(), U256::from(1));
    }

    #[test]
    fn test_order_encode_decode() {
        let order = Order::market_increase(
            [1u8; 32],
            Side::Long,
            U256::from(1000),
            U256::from(5000),
            5,
            None,
            12345,
        );

        let encoded = order.encode();
        let decoded = Order::decode(&mut &encoded[..]).unwrap();

        assert_eq!(decoded.account, [1u8; 32]);
        assert_eq!(decoded.side, Side::Long);
        assert_eq!(decoded.collateral_delta_tokens, U256::from(1000));
    }
}
