//! Self-Trade Prevention (STP) types and logic.
//!
//! Self-Trade Prevention prevents orders from the same user from matching
//! against each other in the order book. This is a critical exchange feature
//! that prevents wash trading.
//!
//! # Modes
//!
//! - `STPMode::None` — No STP checks (default, zero overhead).
//! - `STPMode::CancelTaker` — Cancel the incoming (taker) order on self-trade.
//! - `STPMode::CancelMaker` — Cancel the resting (maker) order and continue matching.
//! - `STPMode::CancelBoth` — Cancel both taker and maker orders.
//!
//! # Bypass
//!
//! Orders with `user_id == Hash32::zero()` (anonymous) always bypass STP checks,
//! regardless of the configured mode.

use pricelevel::{Hash32, Id};
use serde::{Deserialize, Serialize};

/// Self-Trade Prevention mode for the order book.
///
/// Controls what happens when an incoming order would match against a resting
/// order from the same user (identified by [`Hash32`] user ID).
///
/// The default mode is [`STPMode::None`], which disables all STP checks and
/// incurs zero overhead in the matching hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum STPMode {
    /// No self-trade prevention (default). Orders from the same user can
    /// match freely. This mode adds zero overhead to the matching engine.
    #[default]
    None = 0,

    /// Cancel the incoming (taker) order when a self-trade would occur.
    /// Resting orders remain in the book. Partial fills against different
    /// users that precede the self-trade are kept.
    CancelTaker = 1,

    /// Cancel the resting (maker) order(s) from the same user and continue
    /// matching the taker against remaining orders. All same-user resting
    /// orders at each price level are removed before matching proceeds.
    CancelMaker = 2,

    /// Cancel both the incoming (taker) and the resting (maker) order.
    /// Matching stops immediately. Partial fills against different users
    /// that precede the self-trade are kept.
    CancelBoth = 3,
}

impl std::fmt::Display for STPMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            STPMode::None => write!(f, "None"),
            STPMode::CancelTaker => write!(f, "CancelTaker"),
            STPMode::CancelMaker => write!(f, "CancelMaker"),
            STPMode::CancelBoth => write!(f, "CancelBoth"),
        }
    }
}

impl STPMode {
    /// Returns `true` if STP checks are enabled (any mode other than `None`).
    #[must_use]
    #[inline]
    pub fn is_enabled(self) -> bool {
        self != STPMode::None
    }
}

/// Result of an STP check against a single price level.
///
/// Used internally by the matching engine to decide how to proceed
/// after scanning orders at a price level for self-trade conflicts.
#[derive(Debug, Clone)]
pub(crate) enum STPAction {
    /// No self-trade detected at this level; proceed normally.
    NoConflict,

    /// CancelTaker triggered: match up to `safe_quantity` (quantity of
    /// non-same-user orders preceding the first same-user order), then stop.
    CancelTaker {
        /// Maximum quantity that can be safely matched before hitting
        /// a same-user order. Zero means the first order is same-user.
        safe_quantity: u64,
    },

    /// CancelMaker triggered: these maker order IDs should be cancelled
    /// before matching proceeds at this level.
    CancelMaker {
        /// Order IDs of same-user resting orders to cancel.
        maker_order_ids: Vec<Id>,
    },

    /// CancelBoth triggered: match up to `safe_quantity`, then cancel
    /// the maker and stop.
    CancelBoth {
        /// Maximum quantity that can be safely matched before hitting
        /// a same-user order.
        safe_quantity: u64,
        /// The first same-user maker order ID to cancel.
        maker_order_id: Id,
    },
}

/// Scans orders at a price level and determines the STP action.
///
/// # Arguments
/// * `orders` — Resting orders at the price level, in FIFO (time-priority) order.
/// * `taker_user_id` — The user ID of the incoming (taker) order.
/// * `mode` — The active STP mode.
///
/// # Returns
/// The appropriate [`STPAction`] for the matching engine to take.
#[inline]
pub(crate) fn check_stp_at_level(
    orders: &[std::sync::Arc<pricelevel::OrderType<()>>],
    taker_user_id: Hash32,
    mode: STPMode,
) -> STPAction {
    // Fast path: no STP or anonymous taker
    if mode == STPMode::None || taker_user_id == Hash32::zero() {
        return STPAction::NoConflict;
    }

    match mode {
        STPMode::None => STPAction::NoConflict,

        STPMode::CancelTaker => {
            // Find the first same-user order and sum quantity before it
            let mut safe_quantity: u64 = 0;
            for order in orders {
                if order.user_id() == taker_user_id {
                    return STPAction::CancelTaker { safe_quantity };
                }
                // Sum visible quantity of non-same-user orders
                safe_quantity = safe_quantity.saturating_add(order.visible_quantity());
            }
            STPAction::NoConflict
        }

        STPMode::CancelMaker => {
            // Collect all same-user order IDs for cancellation
            let maker_order_ids: Vec<Id> = orders
                .iter()
                .filter(|o| o.user_id() == taker_user_id)
                .map(|o| o.id())
                .collect();

            if maker_order_ids.is_empty() {
                STPAction::NoConflict
            } else {
                STPAction::CancelMaker { maker_order_ids }
            }
        }

        STPMode::CancelBoth => {
            // Find the first same-user order and sum quantity before it
            let mut safe_quantity: u64 = 0;
            for order in orders {
                if order.user_id() == taker_user_id {
                    return STPAction::CancelBoth {
                        safe_quantity,
                        maker_order_id: order.id(),
                    };
                }
                safe_quantity = safe_quantity.saturating_add(order.visible_quantity());
            }
            STPAction::NoConflict
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stp_mode_default_is_none() {
        assert_eq!(STPMode::default(), STPMode::None);
    }

    #[test]
    fn test_stp_mode_is_enabled() {
        assert!(!STPMode::None.is_enabled());
        assert!(STPMode::CancelTaker.is_enabled());
        assert!(STPMode::CancelMaker.is_enabled());
        assert!(STPMode::CancelBoth.is_enabled());
    }

    #[test]
    fn test_stp_mode_display() {
        assert_eq!(STPMode::None.to_string(), "None");
        assert_eq!(STPMode::CancelTaker.to_string(), "CancelTaker");
        assert_eq!(STPMode::CancelMaker.to_string(), "CancelMaker");
        assert_eq!(STPMode::CancelBoth.to_string(), "CancelBoth");
    }

    #[test]
    fn test_check_stp_none_mode_returns_no_conflict() {
        let orders = vec![];
        let action = check_stp_at_level(&orders, Hash32::zero(), STPMode::None);
        assert!(matches!(action, STPAction::NoConflict));
    }

    #[test]
    fn test_check_stp_zero_user_bypasses() {
        let user = Hash32::zero();
        let order = std::sync::Arc::new(pricelevel::OrderType::Standard {
            id: Id::new(),
            price: pricelevel::Price::new(100),
            quantity: pricelevel::Quantity::new(10),
            side: pricelevel::Side::Sell,
            user_id: user,
            timestamp: pricelevel::TimestampMs::new(0),
            time_in_force: pricelevel::TimeInForce::Gtc,
            extra_fields: (),
        });
        let orders = vec![order];
        let action = check_stp_at_level(&orders, user, STPMode::CancelTaker);
        assert!(matches!(action, STPAction::NoConflict));
    }

    #[test]
    fn test_check_stp_cancel_taker_detects_same_user() {
        let user = Hash32::new([1u8; 32]);
        let order = std::sync::Arc::new(pricelevel::OrderType::Standard {
            id: Id::new(),
            price: pricelevel::Price::new(100),
            quantity: pricelevel::Quantity::new(10),
            side: pricelevel::Side::Sell,
            user_id: user,
            timestamp: pricelevel::TimestampMs::new(0),
            time_in_force: pricelevel::TimeInForce::Gtc,
            extra_fields: (),
        });
        let orders = vec![order];
        let action = check_stp_at_level(&orders, user, STPMode::CancelTaker);
        match action {
            STPAction::CancelTaker { safe_quantity } => assert_eq!(safe_quantity, 0),
            _ => panic!("expected CancelTaker action"),
        }
    }

    #[test]
    fn test_check_stp_cancel_taker_safe_quantity_before_self() {
        let taker_user = Hash32::new([1u8; 32]);
        let other_user = Hash32::new([2u8; 32]);

        let other_order = std::sync::Arc::new(pricelevel::OrderType::Standard {
            id: Id::new(),
            price: pricelevel::Price::new(100),
            quantity: pricelevel::Quantity::new(5),
            side: pricelevel::Side::Sell,
            user_id: other_user,
            timestamp: pricelevel::TimestampMs::new(0),
            time_in_force: pricelevel::TimeInForce::Gtc,
            extra_fields: (),
        });
        let same_order = std::sync::Arc::new(pricelevel::OrderType::Standard {
            id: Id::new(),
            price: pricelevel::Price::new(100),
            quantity: pricelevel::Quantity::new(10),
            side: pricelevel::Side::Sell,
            user_id: taker_user,
            timestamp: pricelevel::TimestampMs::new(1),
            time_in_force: pricelevel::TimeInForce::Gtc,
            extra_fields: (),
        });
        let orders = vec![other_order, same_order];
        let action = check_stp_at_level(&orders, taker_user, STPMode::CancelTaker);
        match action {
            STPAction::CancelTaker { safe_quantity } => assert_eq!(safe_quantity, 5),
            _ => panic!("expected CancelTaker action"),
        }
    }

    #[test]
    fn test_check_stp_cancel_maker_collects_ids() {
        let taker_user = Hash32::new([1u8; 32]);
        let other_user = Hash32::new([2u8; 32]);

        let same1 = std::sync::Arc::new(pricelevel::OrderType::Standard {
            id: Id::new(),
            price: pricelevel::Price::new(100),
            quantity: pricelevel::Quantity::new(5),
            side: pricelevel::Side::Sell,
            user_id: taker_user,
            timestamp: pricelevel::TimestampMs::new(0),
            time_in_force: pricelevel::TimeInForce::Gtc,
            extra_fields: (),
        });
        let other = std::sync::Arc::new(pricelevel::OrderType::Standard {
            id: Id::new(),
            price: pricelevel::Price::new(100),
            quantity: pricelevel::Quantity::new(3),
            side: pricelevel::Side::Sell,
            user_id: other_user,
            timestamp: pricelevel::TimestampMs::new(1),
            time_in_force: pricelevel::TimeInForce::Gtc,
            extra_fields: (),
        });
        let same2 = std::sync::Arc::new(pricelevel::OrderType::Standard {
            id: Id::new(),
            price: pricelevel::Price::new(100),
            quantity: pricelevel::Quantity::new(7),
            side: pricelevel::Side::Sell,
            user_id: taker_user,
            timestamp: pricelevel::TimestampMs::new(2),
            time_in_force: pricelevel::TimeInForce::Gtc,
            extra_fields: (),
        });
        let orders = vec![same1.clone(), other, same2.clone()];
        let action = check_stp_at_level(&orders, taker_user, STPMode::CancelMaker);
        match action {
            STPAction::CancelMaker { maker_order_ids } => {
                assert_eq!(maker_order_ids.len(), 2);
                assert_eq!(maker_order_ids[0], same1.id());
                assert_eq!(maker_order_ids[1], same2.id());
            }
            _ => panic!("expected CancelMaker action"),
        }
    }

    #[test]
    fn test_check_stp_cancel_both_detects_self() {
        let user = Hash32::new([1u8; 32]);
        let other_user = Hash32::new([2u8; 32]);

        let other = std::sync::Arc::new(pricelevel::OrderType::Standard {
            id: Id::new(),
            price: pricelevel::Price::new(100),
            quantity: pricelevel::Quantity::new(3),
            side: pricelevel::Side::Sell,
            user_id: other_user,
            timestamp: pricelevel::TimestampMs::new(0),
            time_in_force: pricelevel::TimeInForce::Gtc,
            extra_fields: (),
        });
        let same = std::sync::Arc::new(pricelevel::OrderType::Standard {
            id: Id::new(),
            price: pricelevel::Price::new(100),
            quantity: pricelevel::Quantity::new(10),
            side: pricelevel::Side::Sell,
            user_id: user,
            timestamp: pricelevel::TimestampMs::new(1),
            time_in_force: pricelevel::TimeInForce::Gtc,
            extra_fields: (),
        });
        let orders = vec![other, same.clone()];
        let action = check_stp_at_level(&orders, user, STPMode::CancelBoth);
        match action {
            STPAction::CancelBoth {
                safe_quantity,
                maker_order_id,
            } => {
                assert_eq!(safe_quantity, 3);
                assert_eq!(maker_order_id, same.id());
            }
            _ => panic!("expected CancelBoth action"),
        }
    }

    #[test]
    fn test_check_stp_no_conflict_when_different_users() {
        let taker_user = Hash32::new([1u8; 32]);
        let other_user = Hash32::new([2u8; 32]);

        let order = std::sync::Arc::new(pricelevel::OrderType::Standard {
            id: Id::new(),
            price: pricelevel::Price::new(100),
            quantity: pricelevel::Quantity::new(10),
            side: pricelevel::Side::Sell,
            user_id: other_user,
            timestamp: pricelevel::TimestampMs::new(0),
            time_in_force: pricelevel::TimeInForce::Gtc,
            extra_fields: (),
        });
        let orders = vec![order];

        // All modes should return NoConflict for different users
        assert!(matches!(
            check_stp_at_level(&orders, taker_user, STPMode::CancelTaker),
            STPAction::NoConflict
        ));
        assert!(matches!(
            check_stp_at_level(&orders, taker_user, STPMode::CancelMaker),
            STPAction::NoConflict
        ));
        assert!(matches!(
            check_stp_at_level(&orders, taker_user, STPMode::CancelBoth),
            STPAction::NoConflict
        ));
    }
}
