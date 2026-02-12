//! Order book error types

use pricelevel::{PriceLevelError, Side};
use std::fmt;

/// Errors that can occur within the OrderBook
#[derive(Debug)]
#[non_exhaustive]
pub enum OrderBookError {
    /// Error from underlying price level operations
    PriceLevelError(PriceLevelError),

    /// Order not found in the book
    OrderNotFound(String),

    /// Invalid price level
    InvalidPriceLevel(u128),

    /// Price crossing (bid >= ask)
    PriceCrossing {
        /// Price that would cause crossing
        price: u128,
        /// Side of the order
        side: Side,
        /// Best opposite price
        opposite_price: u128,
    },

    /// Insufficient liquidity for market order
    InsufficientLiquidity {
        /// The side of the market order
        side: Side,
        /// Quantity requested
        requested: u64,
        /// Quantity available
        available: u64,
    },

    /// Operation not permitted for specified order type
    InvalidOperation {
        /// Description of the error
        message: String,
    },

    /// Error while serializing snapshot data
    SerializationError {
        /// Underlying error message
        message: String,
    },

    /// Error while deserializing snapshot data
    DeserializationError {
        /// Underlying error message
        message: String,
    },

    /// Snapshot integrity check failed
    ChecksumMismatch {
        /// Expected checksum value
        expected: String,
        /// Actual checksum value
        actual: String,
    },

    /// Order price is not a multiple of the configured tick size
    InvalidTickSize {
        /// The order price that failed validation
        price: u128,
        /// The configured tick size
        tick_size: u128,
    },

    /// Order quantity is not a multiple of the configured lot size
    InvalidLotSize {
        /// The order quantity that failed validation
        quantity: u64,
        /// The configured lot size
        lot_size: u64,
    },

    /// Order quantity is outside the allowed min/max range
    OrderSizeOutOfRange {
        /// The order quantity that failed validation
        quantity: u64,
        /// The configured minimum order size, if any
        min: Option<u64>,
        /// The configured maximum order size, if any
        max: Option<u64>,
    },

    /// Order rejected because `user_id` is `Hash32::zero()` while
    /// Self-Trade Prevention is enabled. All orders must carry a non-zero
    /// `user_id` when STP mode is active.
    MissingUserId {
        /// The order ID that was rejected
        order_id: pricelevel::OrderId,
    },

    /// Self-trade prevention triggered: the incoming order would have
    /// matched against a resting order from the same user.
    SelfTradePrevented {
        /// The STP mode that was active
        mode: crate::orderbook::stp::STPMode,
        /// The taker (incoming) order ID
        taker_order_id: pricelevel::OrderId,
        /// The user ID that triggered the STP check
        user_id: pricelevel::Hash32,
    },
}

impl fmt::Display for OrderBookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderBookError::PriceLevelError(err) => write!(f, "Price level error: {err}"),
            OrderBookError::OrderNotFound(id) => write!(f, "Order not found: {id}"),
            OrderBookError::InvalidPriceLevel(price) => write!(f, "Invalid price level: {price}"),
            OrderBookError::PriceCrossing {
                price,
                side,
                opposite_price,
            } => {
                write!(
                    f,
                    "Price crossing: {side} {price} would cross opposite at {opposite_price}"
                )
            }
            OrderBookError::InsufficientLiquidity {
                side,
                requested,
                available,
            } => {
                write!(
                    f,
                    "Insufficient liquidity for {side} order: requested {requested}, available {available}"
                )
            }
            OrderBookError::InvalidOperation { message } => {
                write!(f, "Invalid operation: {message}")
            }
            OrderBookError::SerializationError { message } => {
                write!(f, "Serialization error: {message}")
            }
            OrderBookError::DeserializationError { message } => {
                write!(f, "Deserialization error: {message}")
            }
            OrderBookError::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "Checksum mismatch: expected {expected}, but computed {actual}"
                )
            }
            OrderBookError::InvalidTickSize { price, tick_size } => {
                write!(
                    f,
                    "invalid tick size: price {price} is not a multiple of tick size {tick_size}"
                )
            }
            OrderBookError::InvalidLotSize { quantity, lot_size } => {
                write!(
                    f,
                    "invalid lot size: quantity {quantity} is not a multiple of lot size {lot_size}"
                )
            }
            OrderBookError::OrderSizeOutOfRange { quantity, min, max } => {
                write!(
                    f,
                    "order size out of range: quantity {quantity}, min {min:?}, max {max:?}"
                )
            }
            OrderBookError::MissingUserId { order_id } => {
                write!(
                    f,
                    "missing user_id: order {order_id} rejected because STP is enabled and user_id is zero"
                )
            }
            OrderBookError::SelfTradePrevented {
                mode,
                taker_order_id,
                user_id,
            } => {
                write!(
                    f,
                    "self-trade prevented ({mode}): taker {taker_order_id}, user {user_id}"
                )
            }
        }
    }
}

impl std::error::Error for OrderBookError {}

impl From<PriceLevelError> for OrderBookError {
    fn from(err: PriceLevelError) -> Self {
        OrderBookError::PriceLevelError(err)
    }
}
