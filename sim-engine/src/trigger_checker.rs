use crate::messages::{ExecutionType, OrderType, PendingOrderInfo, Price, Side};
use crate::pending_orders::PendingOrder;

pub fn is_triggered(order: &PendingOrder, price: &Price) -> bool {
    let trigger = match order.payload.trigger_price {
        Some(t) => t,
        None => return false,
    };

    check_trigger_condition(
        order.payload.execution_type,
        order.payload.order_type,
        order.payload.side,
        trigger,
        price,
    )
}

pub fn is_triggered_info(order: &PendingOrderInfo, price: &Price) -> bool {
    check_trigger_condition(
        order.execution_type,
        order.order_type,
        order.side,
        order.trigger_price,
        price,
    )
}

fn check_trigger_condition(
    exec_type: ExecutionType,
    order_type: OrderType,
    side: Side,
    trigger: u64,
    price: &Price,
) -> bool {
    match (exec_type, order_type, side) {
        // LIMIT Increase
        (ExecutionType::Limit, OrderType::Increase, Side::Buy) => price.max <= trigger,
        (ExecutionType::Limit, OrderType::Increase, Side::Sell) => price.min >= trigger,

        // LIMIT Decrease
        (ExecutionType::Limit, OrderType::Decrease, Side::Buy) => price.min >= trigger,
        (ExecutionType::Limit, OrderType::Decrease, Side::Sell) => price.max <= trigger,

        // STOP LOSS (Decrease only)
        (ExecutionType::StopLoss, OrderType::Decrease, Side::Buy) => price.min <= trigger,
        (ExecutionType::StopLoss, OrderType::Decrease, Side::Sell) => price.max >= trigger,

        // TAKE PROFIT (Decrease only)
        (ExecutionType::TakeProfit, OrderType::Decrease, Side::Buy) => price.min >= trigger,
        (ExecutionType::TakeProfit, OrderType::Decrease, Side::Sell) => price.max <= trigger,

        // Market — should not be in pending
        (ExecutionType::Market, _, _) => false,

        // Invalid combinations
        _ => false,
    }
}

#[allow(dead_code)]
pub fn passes_slippage_check(order: &PendingOrder, execution_price: u64) -> bool {
    match order.payload.acceptable_price {
        None => true,
        Some(acceptable) => {
            match (order.payload.order_type, order.payload.side) {
                (OrderType::Increase, Side::Buy) | (OrderType::Decrease, Side::Sell) => {
                    execution_price <= acceptable
                }
                (OrderType::Increase, Side::Sell) | (OrderType::Decrease, Side::Buy) => {
                    execution_price >= acceptable
                }
            }
        }
    }
}
