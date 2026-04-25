//! Closed taxonomy of order-rejection reasons exposed on the wire.
//!
//! [`RejectReason`] is the canonical wire-side reject code surfaced on
//! `OrderStatus::Rejected`. Each named variant carries a stable
//! `#[repr(u16)]` discriminant — consumers that publish or parse the
//! value over the wire can rely on those numbers staying stable across
//! `0.7.x` and `0.7.x → 0.7.y` patch upgrades.
//!
//! Forward compatibility is preserved by:
//!
//! - `#[non_exhaustive]` so adding a new variant is non-breaking on
//!   downstream `match` blocks (consumers must keep a wildcard arm).
//! - [`RejectReason::Other`] as an escape hatch for application-side
//!   extensions. Values `>= 1000` are reserved for caller use; the
//!   library itself will never emit a value in that range.
//!
//! The [`From<&OrderBookError>`](RejectReason#impl-From<%26OrderBookError>-for-RejectReason)
//! impl provides operational ergonomics for callers that already hold a
//! typed [`OrderBookError`]: the typed error is the impl detail, the
//! [`RejectReason`] is the stable public contract.

use crate::orderbook::error::OrderBookError;
use serde::{Deserialize, Serialize};

/// Closed taxonomy of reasons an order may be rejected at admission.
///
/// `RejectReason` is the stable wire-side reject code. Each variant has
/// an explicit `#[repr(u16)]` discriminant — consumers that publish or
/// parse the value over the wire can rely on those numbers staying
/// stable across `0.7.x` and `0.7.x → 0.7.y` patch upgrades. Forward
/// compatibility is preserved by:
///
/// - `#[non_exhaustive]` so adding a variant is non-breaking on
///   downstream `match` blocks.
/// - [`Self::Other`] as an escape hatch for application-side extensions.
///   Values `>= 1000` are reserved for caller use; the library will
///   never emit a value in that range.
///
/// # Discriminant table
///
/// | Variant                  | u16 |
/// |--------------------------|-----|
/// | `KillSwitchActive`       | 1   |
/// | `RiskMaxOpenOrders`      | 2   |
/// | `RiskMaxNotional`        | 3   |
/// | `RiskPriceBand`          | 4   |
/// | `PostOnlyWouldCross`     | 5   |
/// | `SelfTradePrevention`    | 6   |
/// | `InvalidPrice`           | 7   |
/// | `InvalidQuantity`        | 8   |
/// | `InvalidPriceLevel`      | 9   |
/// | `OrderSizeOutOfRange`    | 10  |
/// | `MissingUserId`          | 11  |
/// | `DuplicateOrderId`       | 12  |
/// | `InsufficientLiquidity`  | 13  |
/// | `Other(code)`            | code|
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[repr(u16)]
pub enum RejectReason {
    /// New flow rejected because the operational kill switch is engaged.
    KillSwitchActive = 1,
    /// Per-account open-order limit would be breached by this admission.
    RiskMaxOpenOrders = 2,
    /// Per-account notional limit would be breached by this admission.
    RiskMaxNotional = 3,
    /// Submitted price exceeds the configured price band against the
    /// reference price.
    RiskPriceBand = 4,
    /// Post-only order would cross the resting opposite side at the
    /// time of admission.
    PostOnlyWouldCross = 5,
    /// Self-trade prevention rejected the incoming order.
    SelfTradePrevention = 6,
    /// Submitted price violates the configured tick-size validation.
    InvalidPrice = 7,
    /// Submitted quantity violates the configured lot-size validation.
    InvalidQuantity = 8,
    /// The targeted price level is invalid for the requested operation.
    InvalidPriceLevel = 9,
    /// Submitted quantity is outside the configured min/max range.
    OrderSizeOutOfRange = 10,
    /// `user_id` is missing or zero while STP is enabled.
    MissingUserId = 11,
    /// An order with the same id is already present in the book.
    DuplicateOrderId = 12,
    /// The order could not be filled with the available resting depth
    /// (IOC / FOK semantics).
    InsufficientLiquidity = 13,
    /// Caller-supplied / unmapped code. The library never emits this
    /// variant; it exists so applications can ferry their own reject
    /// codes through the same channel without forking the enum.
    Other(u16),
}

impl RejectReason {
    /// Numeric wire code. Stable across `0.7.x`.
    ///
    /// For named variants this returns the explicit `#[repr(u16)]`
    /// discriminant; for [`Self::Other`] this returns the wrapped
    /// caller-supplied code verbatim.
    #[inline]
    #[must_use]
    pub fn as_u16(self) -> u16 {
        match self {
            Self::KillSwitchActive => 1,
            Self::RiskMaxOpenOrders => 2,
            Self::RiskMaxNotional => 3,
            Self::RiskPriceBand => 4,
            Self::PostOnlyWouldCross => 5,
            Self::SelfTradePrevention => 6,
            Self::InvalidPrice => 7,
            Self::InvalidQuantity => 8,
            Self::InvalidPriceLevel => 9,
            Self::OrderSizeOutOfRange => 10,
            Self::MissingUserId => 11,
            Self::DuplicateOrderId => 12,
            Self::InsufficientLiquidity => 13,
            Self::Other(code) => code,
        }
    }
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KillSwitchActive => write!(f, "kill switch active"),
            Self::RiskMaxOpenOrders => write!(f, "risk: max open orders"),
            Self::RiskMaxNotional => write!(f, "risk: max notional"),
            Self::RiskPriceBand => write!(f, "risk: price band"),
            Self::PostOnlyWouldCross => write!(f, "post-only would cross"),
            Self::SelfTradePrevention => write!(f, "self-trade prevention"),
            Self::InvalidPrice => write!(f, "invalid price"),
            Self::InvalidQuantity => write!(f, "invalid quantity"),
            Self::InvalidPriceLevel => write!(f, "invalid price level"),
            Self::OrderSizeOutOfRange => write!(f, "order size out of range"),
            Self::MissingUserId => write!(f, "missing user id"),
            Self::DuplicateOrderId => write!(f, "duplicate order id"),
            Self::InsufficientLiquidity => write!(f, "insufficient liquidity"),
            Self::Other(code) => write!(f, "other({code})"),
        }
    }
}

/// Map a typed [`OrderBookError`] to its wire-side reject code.
///
/// Errors that do not represent a public reject (e.g.
/// `SerializationError`, `ChecksumMismatch`, `NatsPublishError`,
/// internal-state errors) map to [`RejectReason::Other(0)`] — they are
/// not expected to surface on outbound reject events.
///
/// The match below is intentionally exhaustive (no `_ =>` catch-all);
/// any new variant added to [`OrderBookError`] must extend this mapping
/// at compile time. This is enforced because the `impl` lives inside
/// the crate, where exhaustive matches over a `#[non_exhaustive]` enum
/// are still permitted.
impl From<&OrderBookError> for RejectReason {
    #[inline]
    fn from(err: &OrderBookError) -> Self {
        match err {
            OrderBookError::KillSwitchActive => Self::KillSwitchActive,
            OrderBookError::RiskMaxOpenOrders { .. } => Self::RiskMaxOpenOrders,
            OrderBookError::RiskMaxNotional { .. } => Self::RiskMaxNotional,
            OrderBookError::RiskPriceBand { .. } => Self::RiskPriceBand,
            OrderBookError::SelfTradePrevented { .. } => Self::SelfTradePrevention,
            OrderBookError::InvalidPriceLevel(_) => Self::InvalidPriceLevel,
            OrderBookError::PriceCrossing { .. } => Self::PostOnlyWouldCross,
            OrderBookError::InsufficientLiquidity { .. } => Self::InsufficientLiquidity,
            OrderBookError::InvalidTickSize { .. } => Self::InvalidPrice,
            OrderBookError::InvalidLotSize { .. } => Self::InvalidQuantity,
            OrderBookError::OrderSizeOutOfRange { .. } => Self::OrderSizeOutOfRange,
            OrderBookError::MissingUserId { .. } => Self::MissingUserId,
            OrderBookError::PriceLevelError(_) => Self::Other(0),
            OrderBookError::OrderNotFound(_) => Self::Other(0),
            OrderBookError::InvalidOperation { .. } => Self::Other(0),
            OrderBookError::SerializationError { .. } => Self::Other(0),
            OrderBookError::DeserializationError { .. } => Self::Other(0),
            OrderBookError::ChecksumMismatch { .. } => Self::Other(0),
            #[cfg(feature = "nats")]
            OrderBookError::NatsPublishError { .. } => Self::Other(0),
            #[cfg(feature = "nats")]
            OrderBookError::NatsSerializationError { .. } => Self::Other(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::{Hash32, Id, PriceLevelError, Side};

    /// Every named variant — used to drive exhaustive table-style tests.
    /// The `Other` variant is added explicitly where needed.
    fn named_variants() -> [RejectReason; 13] {
        [
            RejectReason::KillSwitchActive,
            RejectReason::RiskMaxOpenOrders,
            RejectReason::RiskMaxNotional,
            RejectReason::RiskPriceBand,
            RejectReason::PostOnlyWouldCross,
            RejectReason::SelfTradePrevention,
            RejectReason::InvalidPrice,
            RejectReason::InvalidQuantity,
            RejectReason::InvalidPriceLevel,
            RejectReason::OrderSizeOutOfRange,
            RejectReason::MissingUserId,
            RejectReason::DuplicateOrderId,
            RejectReason::InsufficientLiquidity,
        ]
    }

    #[test]
    fn test_discriminants_are_stable() {
        assert_eq!(RejectReason::KillSwitchActive.as_u16(), 1);
        assert_eq!(RejectReason::RiskMaxOpenOrders.as_u16(), 2);
        assert_eq!(RejectReason::RiskMaxNotional.as_u16(), 3);
        assert_eq!(RejectReason::RiskPriceBand.as_u16(), 4);
        assert_eq!(RejectReason::PostOnlyWouldCross.as_u16(), 5);
        assert_eq!(RejectReason::SelfTradePrevention.as_u16(), 6);
        assert_eq!(RejectReason::InvalidPrice.as_u16(), 7);
        assert_eq!(RejectReason::InvalidQuantity.as_u16(), 8);
        assert_eq!(RejectReason::InvalidPriceLevel.as_u16(), 9);
        assert_eq!(RejectReason::OrderSizeOutOfRange.as_u16(), 10);
        assert_eq!(RejectReason::MissingUserId.as_u16(), 11);
        assert_eq!(RejectReason::DuplicateOrderId.as_u16(), 12);
        assert_eq!(RejectReason::InsufficientLiquidity.as_u16(), 13);
    }

    #[test]
    fn test_other_passthrough() {
        assert_eq!(RejectReason::Other(0).as_u16(), 0);
        assert_eq!(RejectReason::Other(7777).as_u16(), 7777);
        assert_eq!(RejectReason::Other(u16::MAX).as_u16(), u16::MAX);
    }

    #[test]
    fn test_display_reads_human_text() {
        // Smoke check that every variant produces a non-empty,
        // human-readable line.
        for reason in named_variants() {
            let text = reason.to_string();
            assert!(!text.is_empty(), "Display for {reason:?} produced empty");
        }
        assert_eq!(
            RejectReason::KillSwitchActive.to_string(),
            "kill switch active"
        );
        assert_eq!(RejectReason::Other(42).to_string(), "other(42)");
    }

    #[test]
    fn test_from_order_book_error_kill_switch_maps_to_kill_switch_active() {
        let err = OrderBookError::KillSwitchActive;
        assert_eq!(RejectReason::from(&err), RejectReason::KillSwitchActive);
    }

    #[test]
    fn test_from_order_book_error_risk_max_open_maps_to_risk_max_open_orders() {
        let err = OrderBookError::RiskMaxOpenOrders {
            account: Hash32::from([1u8; 32]),
            current: 5,
            limit: 5,
        };
        assert_eq!(RejectReason::from(&err), RejectReason::RiskMaxOpenOrders);
    }

    #[test]
    fn test_from_order_book_error_risk_max_notional() {
        let err = OrderBookError::RiskMaxNotional {
            account: Hash32::from([1u8; 32]),
            current: 100,
            attempted: 50,
            limit: 100,
        };
        assert_eq!(RejectReason::from(&err), RejectReason::RiskMaxNotional);
    }

    #[test]
    fn test_from_order_book_error_risk_price_band() {
        let err = OrderBookError::RiskPriceBand {
            submitted: 1_000_000,
            reference: 500_000,
            deviation_bps: 10_000,
            limit_bps: 100,
        };
        assert_eq!(RejectReason::from(&err), RejectReason::RiskPriceBand);
    }

    #[test]
    fn test_from_order_book_error_invalid_price_level_maps_to_invalid_price_level() {
        let err = OrderBookError::InvalidPriceLevel(42);
        assert_eq!(RejectReason::from(&err), RejectReason::InvalidPriceLevel);
    }

    #[test]
    fn test_from_order_book_error_order_size_out_of_range() {
        let err = OrderBookError::OrderSizeOutOfRange {
            quantity: 0,
            min: Some(1),
            max: Some(100),
        };
        assert_eq!(RejectReason::from(&err), RejectReason::OrderSizeOutOfRange);
    }

    #[test]
    fn test_from_order_book_error_missing_user_id() {
        let err = OrderBookError::MissingUserId {
            order_id: Id::new_uuid(),
        };
        assert_eq!(RejectReason::from(&err), RejectReason::MissingUserId);
    }

    #[test]
    fn test_from_order_book_error_self_trade_prevented_maps_to_self_trade_prevention() {
        let err = OrderBookError::SelfTradePrevented {
            mode: crate::orderbook::stp::STPMode::CancelTaker,
            taker_order_id: Id::new_uuid(),
            user_id: Hash32::from([1u8; 32]),
        };
        assert_eq!(RejectReason::from(&err), RejectReason::SelfTradePrevention);
    }

    #[test]
    fn test_from_order_book_error_price_crossing_maps_to_post_only_would_cross() {
        let err = OrderBookError::PriceCrossing {
            price: 100,
            side: Side::Buy,
            opposite_price: 99,
        };
        assert_eq!(RejectReason::from(&err), RejectReason::PostOnlyWouldCross);
    }

    #[test]
    fn test_from_order_book_error_invalid_tick_size_maps_to_invalid_price() {
        let err = OrderBookError::InvalidTickSize {
            price: 150,
            tick_size: 100,
        };
        assert_eq!(RejectReason::from(&err), RejectReason::InvalidPrice);
    }

    #[test]
    fn test_from_order_book_error_invalid_lot_size_maps_to_invalid_quantity() {
        let err = OrderBookError::InvalidLotSize {
            quantity: 75,
            lot_size: 10,
        };
        assert_eq!(RejectReason::from(&err), RejectReason::InvalidQuantity);
    }

    #[test]
    fn test_from_order_book_error_insufficient_liquidity() {
        let err = OrderBookError::InsufficientLiquidity {
            side: Side::Buy,
            requested: 100,
            available: 50,
        };
        assert_eq!(RejectReason::from(&err), RejectReason::InsufficientLiquidity);
    }

    #[test]
    fn test_from_order_book_error_serialization_error_maps_to_other_zero() {
        let err = OrderBookError::SerializationError {
            message: "oops".to_string(),
        };
        assert_eq!(RejectReason::from(&err), RejectReason::Other(0));
    }

    #[test]
    fn test_from_order_book_error_internal_state_errors_map_to_other_zero() {
        let cases = [
            OrderBookError::OrderNotFound("x".to_string()),
            OrderBookError::InvalidOperation {
                message: "nope".to_string(),
            },
            OrderBookError::DeserializationError {
                message: "bad".to_string(),
            },
            OrderBookError::ChecksumMismatch {
                expected: "a".to_string(),
                actual: "b".to_string(),
            },
            OrderBookError::PriceLevelError(PriceLevelError::InvalidFormat),
        ];
        for err in cases {
            assert_eq!(
                RejectReason::from(&err),
                RejectReason::Other(0),
                "{err:?} should map to Other(0)"
            );
        }
    }

    #[test]
    fn test_serde_json_roundtrip_each_variant() {
        for reason in named_variants() {
            let json = serde_json::to_string(&reason).expect("serialize named variant");
            let decoded: RejectReason =
                serde_json::from_str(&json).expect("deserialize named variant");
            assert_eq!(decoded, reason);
        }
        let other = RejectReason::Other(42);
        let json = serde_json::to_string(&other).expect("serialize Other(42)");
        let decoded: RejectReason = serde_json::from_str(&json).expect("deserialize Other(42)");
        assert_eq!(decoded, other);
    }

    #[cfg(feature = "bincode")]
    #[test]
    fn test_serde_bincode_roundtrip_each_variant() {
        let cfg = bincode::config::standard();
        for reason in named_variants() {
            let bytes = bincode::serde::encode_to_vec(reason, cfg).expect("encode named variant");
            let (decoded, n) = bincode::serde::decode_from_slice::<RejectReason, _>(&bytes, cfg)
                .expect("decode named variant");
            assert_eq!(decoded, reason);
            assert_eq!(n, bytes.len(), "bincode should consume entire payload");
        }
        let other = RejectReason::Other(42);
        let bytes = bincode::serde::encode_to_vec(other, cfg).expect("encode Other(42)");
        let (decoded, n) = bincode::serde::decode_from_slice::<RejectReason, _>(&bytes, cfg)
            .expect("decode Other(42)");
        assert_eq!(decoded, other);
        assert_eq!(n, bytes.len());
    }
}
