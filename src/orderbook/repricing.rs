//! Re-pricing logic for special order types (PeggedOrder and TrailingStop)
//!
//! This module provides automatic price adjustment for:
//! - **PeggedOrder**: Orders that track a reference price (best bid, best ask, mid price, or last trade)
//! - **TrailingStop**: Orders that follow the market price with a fixed trail amount
//!
//! # Example
//!
//! ```ignore
//! use orderbook_rs::OrderBook;
//!
//! let book = OrderBook::<()>::new("BTC/USD");
//!
//! // Add a pegged order that tracks best bid with +5 offset
//! // When best bid changes, the order price will be automatically adjusted
//!
//! // Add a trailing stop that trails by 10 units
//! // When market moves favorably, the stop price adjusts automatically
//!
//! // Trigger re-pricing after market changes
//! book.reprice_special_orders();
//! ```

use crate::orderbook::error::OrderBookError;
use dashmap::DashSet;
use pricelevel::{Id, OrderType, PegReferenceType, Side};
use tracing::trace;

/// Tracks special orders that require re-pricing
#[derive(Debug, Default)]
pub struct SpecialOrderTracker {
    /// Order IDs of pegged orders that need re-pricing when reference prices change
    pegged_orders: DashSet<Id>,
    /// Order IDs of trailing stop orders that need re-pricing when market moves
    trailing_stop_orders: DashSet<Id>,
}

impl SpecialOrderTracker {
    /// Creates a new empty tracker
    pub fn new() -> Self {
        Self {
            pegged_orders: DashSet::new(),
            trailing_stop_orders: DashSet::new(),
        }
    }

    /// Registers a pegged order for tracking
    pub fn register_pegged_order(&self, order_id: Id) {
        self.pegged_orders.insert(order_id);
        trace!("Registered pegged order {} for re-pricing", order_id);
    }

    /// Registers a trailing stop order for tracking
    pub fn register_trailing_stop(&self, order_id: Id) {
        self.trailing_stop_orders.insert(order_id);
        trace!("Registered trailing stop order {} for re-pricing", order_id);
    }

    /// Unregisters a pegged order (e.g., when cancelled or filled)
    pub fn unregister_pegged_order(&self, order_id: &Id) {
        self.pegged_orders.remove(order_id);
        trace!("Unregistered pegged order {} from re-pricing", order_id);
    }

    /// Unregisters a trailing stop order (e.g., when cancelled or filled)
    pub fn unregister_trailing_stop(&self, order_id: &Id) {
        self.trailing_stop_orders.remove(order_id);
        trace!(
            "Unregistered trailing stop order {} from re-pricing",
            order_id
        );
    }

    /// Returns the number of tracked pegged orders
    pub fn pegged_order_count(&self) -> usize {
        self.pegged_orders.len()
    }

    /// Returns the number of tracked trailing stop orders
    pub fn trailing_stop_count(&self) -> usize {
        self.trailing_stop_orders.len()
    }

    /// Returns all tracked pegged order IDs
    pub fn pegged_order_ids(&self) -> Vec<Id> {
        self.pegged_orders.iter().map(|r| *r).collect()
    }

    /// Returns all tracked trailing stop order IDs
    pub fn trailing_stop_ids(&self) -> Vec<Id> {
        self.trailing_stop_orders.iter().map(|r| *r).collect()
    }

    /// Clears all tracked orders
    pub fn clear(&self) {
        self.pegged_orders.clear();
        self.trailing_stop_orders.clear();
    }
}

/// Result of a re-pricing operation
#[derive(Debug, Clone, Default)]
pub struct RepricingResult {
    /// Number of pegged orders that were re-priced
    pub pegged_orders_repriced: usize,
    /// Number of trailing stops that were re-priced
    pub trailing_stops_repriced: usize,
    /// Order IDs that failed to re-price
    pub failed_orders: Vec<(Id, String)>,
}

/// Calculates the new price for a pegged order based on reference price
///
/// # Arguments
/// * `reference_type` - The type of reference price to use
/// * `offset` - The offset from the reference price (can be negative)
/// * `side` - The side of the order (Buy or Sell)
/// * `best_bid` - Current best bid price
/// * `best_ask` - Current best ask price
/// * `mid_price` - Current mid price in integer units
/// * `last_trade` - Last trade price
///
/// # Returns
/// The calculated new price, or None if reference price is not available
pub fn calculate_pegged_price(
    reference_type: PegReferenceType,
    offset: i64,
    _side: Side,
    best_bid: Option<u128>,
    best_ask: Option<u128>,
    mid_price: Option<u128>,
    last_trade: Option<u128>,
) -> Option<u128> {
    let reference_price = match reference_type {
        PegReferenceType::BestBid => best_bid?,
        PegReferenceType::BestAsk => best_ask?,
        PegReferenceType::MidPrice => mid_price?,
        PegReferenceType::LastTrade => last_trade?,
    };

    // Apply offset (can be positive or negative)
    let new_price = if offset >= 0 {
        reference_price.saturating_add(offset as u128)
    } else {
        reference_price.saturating_sub((-offset) as u128)
    };

    // Ensure price is at least 1
    Some(new_price.max(1))
}

/// Calculates the new price for a trailing stop order
///
/// For a **BUY** trailing stop (used to enter long or cover short):
/// - The stop price trails ABOVE the market low
/// - When market falls, stop price falls with it (maintaining trail_amount above)
/// - When market rises, stop price stays (doesn't rise)
/// - Triggers when market rises to meet the stop price
///
/// For a **SELL** trailing stop (used to exit long or enter short):
/// - The stop price trails BELOW the market high
/// - When market rises, stop price rises with it (maintaining trail_amount below)
/// - When market falls, stop price stays (doesn't fall)
/// - Triggers when market falls to meet the stop price
///
/// # Arguments
/// * `side` - The side of the order (Buy or Sell)
/// * `current_stop_price` - Current stop price of the order
/// * `trail_amount` - The trailing amount (distance from reference price)
/// * `last_reference_price` - The last reference price used for calculation
/// * `current_market_price` - Current market price (best bid for sell, best ask for buy)
///
/// # Returns
/// A tuple of (new_stop_price, new_reference_price) if adjustment is needed, None otherwise
pub fn calculate_trailing_stop_price(
    side: Side,
    current_stop_price: u128,
    trail_amount: u64,
    last_reference_price: u128,
    current_market_price: u128,
) -> Option<(u128, u128)> {
    let trail = trail_amount as u128;

    match side {
        Side::Sell => {
            // Sell trailing stop: trails below the market high
            // Only adjust upward when market makes new highs
            if current_market_price > last_reference_price {
                // Market made a new high, adjust stop price upward
                let new_stop_price = current_market_price.saturating_sub(trail);
                if new_stop_price > current_stop_price {
                    return Some((new_stop_price, current_market_price));
                }
            }
        }
        Side::Buy => {
            // Buy trailing stop: trails above the market low
            // Only adjust downward when market makes new lows
            if current_market_price < last_reference_price {
                // Market made a new low, adjust stop price downward
                let new_stop_price = current_market_price.saturating_add(trail);
                if new_stop_price < current_stop_price {
                    return Some((new_stop_price, current_market_price));
                }
            }
        }
    }

    None
}

/// Extension trait for OrderBook to support re-pricing operations
pub trait RepricingOperations<T> {
    /// Re-prices all pegged orders based on current market conditions
    ///
    /// This should be called when:
    /// - Best bid changes
    /// - Best ask changes
    /// - A trade occurs (for LastTrade pegged orders)
    fn reprice_pegged_orders(&self) -> Result<usize, OrderBookError>;

    /// Re-prices all trailing stop orders based on current market conditions
    ///
    /// This should be called when:
    /// - Market price moves (after each trade)
    fn reprice_trailing_stops(&self) -> Result<usize, OrderBookError>;

    /// Re-prices all special orders (both pegged and trailing stops)
    ///
    /// Convenience method that calls both `reprice_pegged_orders` and `reprice_trailing_stops`
    fn reprice_special_orders(&self) -> Result<RepricingResult, OrderBookError>;

    /// Checks if a trailing stop order should be triggered
    ///
    /// # Arguments
    /// * `order` - The trailing stop order to check
    /// * `current_market_price` - Current market price
    ///
    /// # Returns
    /// `true` if the order should be triggered (converted to market order)
    fn should_trigger_trailing_stop(
        &self,
        order: &OrderType<T>,
        current_market_price: u128,
    ) -> bool;
}

// Implementation is in book.rs to avoid circular dependencies

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_pegged_price_best_bid() {
        let price = calculate_pegged_price(
            PegReferenceType::BestBid,
            5,
            Side::Buy,
            Some(100),
            Some(105),
            Some(102),
            Some(101),
        );
        assert_eq!(price, Some(105)); // 100 + 5
    }

    #[test]
    fn test_calculate_pegged_price_best_ask() {
        let price = calculate_pegged_price(
            PegReferenceType::BestAsk,
            -3,
            Side::Sell,
            Some(100),
            Some(105),
            Some(102),
            Some(101),
        );
        assert_eq!(price, Some(102)); // 105 - 3
    }

    #[test]
    fn test_calculate_pegged_price_mid_price() {
        let price = calculate_pegged_price(
            PegReferenceType::MidPrice,
            0,
            Side::Buy,
            Some(100),
            Some(110),
            Some(105),
            Some(103),
        );
        assert_eq!(price, Some(105)); // mid price (105)
    }

    #[test]
    fn test_calculate_pegged_price_last_trade() {
        let price = calculate_pegged_price(
            PegReferenceType::LastTrade,
            2,
            Side::Buy,
            Some(100),
            Some(105),
            Some(102),
            Some(101),
        );
        assert_eq!(price, Some(103)); // 101 + 2
    }

    #[test]
    fn test_calculate_pegged_price_missing_reference() {
        let price = calculate_pegged_price(
            PegReferenceType::BestBid,
            5,
            Side::Buy,
            None, // No best bid
            Some(105),
            Some(102),
            Some(101),
        );
        assert_eq!(price, None);
    }

    #[test]
    fn test_calculate_pegged_price_negative_offset_floor() {
        let price = calculate_pegged_price(
            PegReferenceType::BestBid,
            -200, // Large negative offset
            Side::Buy,
            Some(100),
            Some(105),
            Some(102),
            Some(101),
        );
        assert_eq!(price, Some(1)); // Minimum price is 1
    }

    #[test]
    fn test_trailing_stop_sell_market_rises() {
        // Sell trailing stop: market rises from 100 to 110
        // Stop should adjust from 95 to 105 (maintaining 5 unit trail)
        let result = calculate_trailing_stop_price(
            Side::Sell,
            95,  // current stop
            5,   // trail amount
            100, // last reference (market was at 100)
            110, // current market (market rose to 110)
        );
        assert_eq!(result, Some((105, 110))); // new stop = 110 - 5 = 105
    }

    #[test]
    fn test_trailing_stop_sell_market_falls() {
        // Sell trailing stop: market falls from 100 to 90
        // Stop should NOT adjust (stays at 95)
        let result = calculate_trailing_stop_price(
            Side::Sell,
            95,  // current stop
            5,   // trail amount
            100, // last reference
            90,  // current market (fell)
        );
        assert_eq!(result, None); // No adjustment
    }

    #[test]
    fn test_trailing_stop_buy_market_falls() {
        // Buy trailing stop: market falls from 100 to 90
        // Stop should adjust from 105 to 95 (maintaining 5 unit trail)
        let result = calculate_trailing_stop_price(
            Side::Buy,
            105, // current stop
            5,   // trail amount
            100, // last reference (market was at 100)
            90,  // current market (market fell to 90)
        );
        assert_eq!(result, Some((95, 90))); // new stop = 90 + 5 = 95
    }

    #[test]
    fn test_trailing_stop_buy_market_rises() {
        // Buy trailing stop: market rises from 100 to 110
        // Stop should NOT adjust (stays at 105)
        let result = calculate_trailing_stop_price(
            Side::Buy,
            105, // current stop
            5,   // trail amount
            100, // last reference
            110, // current market (rose)
        );
        assert_eq!(result, None); // No adjustment
    }

    #[test]
    fn test_special_order_tracker() {
        let tracker = SpecialOrderTracker::new();

        let id1 = Id::from_u64(1);
        let id2 = Id::from_u64(2);
        let id3 = Id::from_u64(3);

        // Register orders
        tracker.register_pegged_order(id1);
        tracker.register_pegged_order(id2);
        tracker.register_trailing_stop(id3);

        assert_eq!(tracker.pegged_order_count(), 2);
        assert_eq!(tracker.trailing_stop_count(), 1);

        // Unregister
        tracker.unregister_pegged_order(&id1);
        assert_eq!(tracker.pegged_order_count(), 1);

        // Clear
        tracker.clear();
        assert_eq!(tracker.pegged_order_count(), 0);
        assert_eq!(tracker.trailing_stop_count(), 0);
    }
}
