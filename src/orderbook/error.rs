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

    /// New flow (submit / modify / replace) is rejected because the
    /// kill switch is engaged. Cancel and mass-cancel paths still
    /// operate so operators can drain the book in an orderly way.
    KillSwitchActive,

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
        order_id: pricelevel::Id,
    },

    /// Self-trade prevention triggered: the incoming order would have
    /// matched against a resting order from the same user.
    SelfTradePrevented {
        /// The STP mode that was active
        mode: crate::orderbook::stp::STPMode,
        /// The taker (incoming) order ID
        taker_order_id: pricelevel::Id,
        /// The user ID that triggered the STP check
        user_id: pricelevel::Hash32,
    },

    /// Failed to publish a trade event to NATS JetStream.
    #[cfg(feature = "nats")]
    NatsPublishError {
        /// Description of the publish failure
        message: String,
    },

    /// Failed to serialize a trade event for NATS publishing.
    #[cfg(feature = "nats")]
    NatsSerializationError {
        /// Description of the serialization failure
        message: String,
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
            OrderBookError::KillSwitchActive => {
                write!(f, "kill switch active: new order entry is halted")
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
            #[cfg(feature = "nats")]
            OrderBookError::NatsPublishError { message } => {
                write!(f, "nats publish error: {message}")
            }
            #[cfg(feature = "nats")]
            OrderBookError::NatsSerializationError { message } => {
                write!(f, "nats serialization error: {message}")
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

impl Clone for OrderBookError {
    fn clone(&self) -> Self {
        match self {
            OrderBookError::PriceLevelError(err) => {
                // PriceLevelError doesn't implement Clone, so we manually clone each variant
                let cloned_err = match err {
                    PriceLevelError::ParseError { message } => PriceLevelError::ParseError {
                        message: message.clone(),
                    },
                    PriceLevelError::InvalidFormat => PriceLevelError::InvalidFormat,
                    PriceLevelError::UnknownOrderType(s) => {
                        PriceLevelError::UnknownOrderType(s.clone())
                    }
                    PriceLevelError::MissingField(s) => PriceLevelError::MissingField(s.clone()),
                    PriceLevelError::InvalidFieldValue { field, value } => {
                        PriceLevelError::InvalidFieldValue {
                            field: field.clone(),
                            value: value.clone(),
                        }
                    }
                    PriceLevelError::InvalidOperation { message } => {
                        PriceLevelError::InvalidOperation {
                            message: message.clone(),
                        }
                    }
                    PriceLevelError::SerializationError { message } => {
                        PriceLevelError::SerializationError {
                            message: message.clone(),
                        }
                    }
                    PriceLevelError::DeserializationError { message } => {
                        PriceLevelError::DeserializationError {
                            message: message.clone(),
                        }
                    }
                    PriceLevelError::ChecksumMismatch { expected, actual } => {
                        PriceLevelError::ChecksumMismatch {
                            expected: expected.clone(),
                            actual: actual.clone(),
                        }
                    }
                };
                OrderBookError::PriceLevelError(cloned_err)
            }
            OrderBookError::OrderNotFound(s) => OrderBookError::OrderNotFound(s.clone()),
            OrderBookError::InvalidPriceLevel(p) => OrderBookError::InvalidPriceLevel(*p),
            OrderBookError::PriceCrossing {
                price,
                side,
                opposite_price,
            } => OrderBookError::PriceCrossing {
                price: *price,
                side: *side,
                opposite_price: *opposite_price,
            },
            OrderBookError::InsufficientLiquidity {
                side,
                requested,
                available,
            } => OrderBookError::InsufficientLiquidity {
                side: *side,
                requested: *requested,
                available: *available,
            },
            OrderBookError::InvalidOperation { message } => OrderBookError::InvalidOperation {
                message: message.clone(),
            },
            OrderBookError::KillSwitchActive => OrderBookError::KillSwitchActive,
            OrderBookError::SerializationError { message } => OrderBookError::SerializationError {
                message: message.clone(),
            },
            OrderBookError::DeserializationError { message } => {
                OrderBookError::DeserializationError {
                    message: message.clone(),
                }
            }
            OrderBookError::ChecksumMismatch { expected, actual } => {
                OrderBookError::ChecksumMismatch {
                    expected: expected.clone(),
                    actual: actual.clone(),
                }
            }
            OrderBookError::InvalidTickSize { price, tick_size } => {
                OrderBookError::InvalidTickSize {
                    price: *price,
                    tick_size: *tick_size,
                }
            }
            OrderBookError::InvalidLotSize { quantity, lot_size } => {
                OrderBookError::InvalidLotSize {
                    quantity: *quantity,
                    lot_size: *lot_size,
                }
            }
            OrderBookError::OrderSizeOutOfRange { quantity, min, max } => {
                OrderBookError::OrderSizeOutOfRange {
                    quantity: *quantity,
                    min: *min,
                    max: *max,
                }
            }
            OrderBookError::MissingUserId { order_id } => OrderBookError::MissingUserId {
                order_id: *order_id,
            },
            OrderBookError::SelfTradePrevented {
                mode,
                taker_order_id,
                user_id,
            } => OrderBookError::SelfTradePrevented {
                mode: *mode,
                taker_order_id: *taker_order_id,
                user_id: *user_id,
            },
            #[cfg(feature = "nats")]
            OrderBookError::NatsPublishError { message } => OrderBookError::NatsPublishError {
                message: message.clone(),
            },
            #[cfg(feature = "nats")]
            OrderBookError::NatsSerializationError { message } => {
                OrderBookError::NatsSerializationError {
                    message: message.clone(),
                }
            }
        }
    }
}

/// Errors that can occur in BookManager operations
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ManagerError {
    /// Trade processor has already been started
    ProcessorAlreadyStarted,
}

impl fmt::Display for ManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManagerError::ProcessorAlreadyStarted => {
                write!(f, "trade processor already started")
            }
        }
    }
}

impl std::error::Error for ManagerError {}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::{Hash32, Id};

    #[test]
    fn test_clone_order_not_found() {
        let error = OrderBookError::OrderNotFound("order123".to_string());
        let cloned = error.clone();
        assert!(matches!(cloned, OrderBookError::OrderNotFound(ref s) if s == "order123"));
    }

    #[test]
    fn test_clone_invalid_price_level() {
        let error = OrderBookError::InvalidPriceLevel(12345);
        let cloned = error.clone();
        assert!(matches!(cloned, OrderBookError::InvalidPriceLevel(12345)));
    }

    #[test]
    fn test_clone_price_crossing() {
        let error = OrderBookError::PriceCrossing {
            price: 100,
            side: Side::Buy,
            opposite_price: 99,
        };
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::PriceCrossing {
                price: 100,
                side: Side::Buy,
                opposite_price: 99
            }
        ));
    }

    #[test]
    fn test_clone_insufficient_liquidity() {
        let error = OrderBookError::InsufficientLiquidity {
            side: Side::Sell,
            requested: 1000,
            available: 500,
        };
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::InsufficientLiquidity {
                side: Side::Sell,
                requested: 1000,
                available: 500
            }
        ));
    }

    #[test]
    fn test_clone_invalid_operation() {
        let error = OrderBookError::InvalidOperation {
            message: "Cannot cancel filled order".to_string(),
        };
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::InvalidOperation { ref message } if message == "Cannot cancel filled order"
        ));
    }

    #[test]
    fn test_clone_serialization_error() {
        let error = OrderBookError::SerializationError {
            message: "Failed to serialize".to_string(),
        };
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::SerializationError { ref message } if message == "Failed to serialize"
        ));
    }

    #[test]
    fn test_clone_checksum_mismatch() {
        let error = OrderBookError::ChecksumMismatch {
            expected: "abc123".to_string(),
            actual: "def456".to_string(),
        };
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::ChecksumMismatch { ref expected, ref actual }
            if expected == "abc123" && actual == "def456"
        ));
    }

    #[test]
    fn test_clone_invalid_tick_size() {
        let error = OrderBookError::InvalidTickSize {
            price: 10050,
            tick_size: 100,
        };
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::InvalidTickSize {
                price: 10050,
                tick_size: 100
            }
        ));
    }

    #[test]
    fn test_clone_invalid_lot_size() {
        let error = OrderBookError::InvalidLotSize {
            quantity: 75,
            lot_size: 10,
        };
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::InvalidLotSize {
                quantity: 75,
                lot_size: 10
            }
        ));
    }

    #[test]
    fn test_clone_order_size_out_of_range() {
        let error = OrderBookError::OrderSizeOutOfRange {
            quantity: 5,
            min: Some(10),
            max: Some(1000),
        };
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::OrderSizeOutOfRange {
                quantity: 5,
                min: Some(10),
                max: Some(1000)
            }
        ));
    }

    #[test]
    fn test_clone_missing_user_id() {
        let order_id = Id::new_uuid();
        let error = OrderBookError::MissingUserId { order_id };
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::MissingUserId { order_id: id } if id == order_id
        ));
    }

    #[test]
    fn test_clone_self_trade_prevented() {
        let taker_id = Id::new_uuid();
        let user_id = Hash32::from([1u8; 32]);
        let error = OrderBookError::SelfTradePrevented {
            mode: crate::orderbook::stp::STPMode::CancelMaker,
            taker_order_id: taker_id,
            user_id,
        };
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::SelfTradePrevented {
                mode: crate::orderbook::stp::STPMode::CancelMaker,
                taker_order_id: id,
                user_id: uid
            } if id == taker_id && uid == user_id
        ));
    }

    #[test]
    fn test_clone_price_level_error_parse_error() {
        let price_level_err = PriceLevelError::ParseError {
            message: "Parse failed".to_string(),
        };
        let error = OrderBookError::PriceLevelError(price_level_err);
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::PriceLevelError(PriceLevelError::ParseError { ref message })
            if message == "Parse failed"
        ));
    }

    #[test]
    fn test_clone_price_level_error_invalid_format() {
        let price_level_err = PriceLevelError::InvalidFormat;
        let error = OrderBookError::PriceLevelError(price_level_err);
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::PriceLevelError(PriceLevelError::InvalidFormat)
        ));
    }

    #[test]
    fn test_clone_price_level_error_unknown_order_type() {
        let price_level_err = PriceLevelError::UnknownOrderType("CUSTOM".to_string());
        let error = OrderBookError::PriceLevelError(price_level_err);
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::PriceLevelError(PriceLevelError::UnknownOrderType(ref s))
            if s == "CUSTOM"
        ));
    }

    #[test]
    fn test_clone_price_level_error_checksum_mismatch() {
        let price_level_err = PriceLevelError::ChecksumMismatch {
            expected: "hash1".to_string(),
            actual: "hash2".to_string(),
        };
        let error = OrderBookError::PriceLevelError(price_level_err);
        let cloned = error.clone();
        assert!(matches!(
            cloned,
            OrderBookError::PriceLevelError(PriceLevelError::ChecksumMismatch {
                ref expected,
                ref actual
            }) if expected == "hash1" && actual == "hash2"
        ));
    }
}
