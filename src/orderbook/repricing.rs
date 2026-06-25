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

    /// Returns all tracked pegged order IDs in a deterministic order.
    ///
    /// The underlying [`DashSet`] iterates in an unspecified order, which would
    /// make the re-pricing sequence (and any events / journal entries it
    /// produces) non-reproducible across runs and break replay determinism and
    /// price-time tie-breaking on re-insert. `Id` does not implement `Ord`, so
    /// we sort by the deterministic `Display`/`to_string` key. The per-id
    /// `to_string` allocation is acceptable here: this is off the matching hot
    /// path (operator-triggered maintenance, not per-submit).
    pub fn pegged_order_ids(&self) -> Vec<Id> {
        let mut ids: Vec<Id> = self.pegged_orders.iter().map(|r| *r).collect();
        ids.sort_by_key(|id| id.to_string());
        ids
    }

    /// Returns all tracked trailing stop order IDs in a deterministic order.
    ///
    /// See [`pegged_order_ids`](Self::pegged_order_ids) for why the order is
    /// sorted by the `Display`/`to_string` key rather than relying on
    /// [`DashSet`] iteration order.
    pub fn trailing_stop_ids(&self) -> Vec<Id> {
        let mut ids: Vec<Id> = self.trailing_stop_orders.iter().map(|r| *r).collect();
        ids.sort_by_key(|id| id.to_string());
        ids
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

/// Calculates the new price for a pegged order based on reference price.
///
/// The computed `reference ± offset` price is **clamped to the passive side of
/// the market** so a pegged re-price can never cross the spread and trade
/// aggressively during a maintenance operation:
/// - A **Buy** peg is capped strictly **below** the best ask (one tick inside,
///   `best_ask - tick`); a buy resting at/above the best ask would cross.
/// - A **Sell** peg is floored strictly **above** the best bid (one tick
///   inside, `best_bid + tick`); a sell resting at/below the best bid would
///   cross.
///
/// After clamping the price is repriced via `UpdatePrice` -> `add_order`, so it
/// rests passively just inside the spread instead of crossing.
///
/// Tick alignment: `best_bid` / `best_ask` are price-level keys and therefore
/// tick-aligned, but the clamp bound and the raw `reference ± offset` price are
/// not. When `tick_size > 1` the clamped price is **snapped onto the tick grid
/// in the passive direction** (round *down* for a Buy, *up* for a Sell) so the
/// re-priced order is always restable. This matters because `add_order`
/// validates tick alignment and the re-price path swallows that error, so an
/// off-tick price would silently abort the re-price and leave the peg at a stale
/// price. If, after snapping and flooring to the minimum valid price, the price
/// would still cross the touch (degenerate cases such as `best_ask == tick`),
/// there is no valid passive resting price this cycle and the function returns
/// `None` — the re-price is skipped rather than crossing or resting off-book.
///
/// # Arguments
/// * `reference_type` - The type of reference price to use
/// * `offset` - The offset from the reference price (can be negative)
/// * `side` - The side of the order (Buy or Sell); drives the passive-side clamp
/// * `best_bid` - Current best bid price
/// * `best_ask` - Current best ask price
/// * `mid_price` - Current mid price in integer units
/// * `last_trade` - Last trade price
/// * `tick_size` - Book tick size, if any; drives passive-side snapping and the
///   minimum valid resting price. `None` (or `<= 1`) preserves non-tick-book
///   behavior (effective step of 1)
///
/// # Returns
/// The calculated new price, or `None` if the reference price is unavailable or
/// no valid passive resting price exists this cycle
// Each argument is a distinct, independently-sourced market input (reference
// type, offset, side, the four reference prices, tick size); bundling them into
// a struct would add ceremony without clarifying the pure calculation. Matches
// the convention used by other multi-input helpers in this crate.
#[allow(clippy::too_many_arguments)]
pub fn calculate_pegged_price(
    reference_type: PegReferenceType,
    offset: i64,
    side: Side,
    best_bid: Option<u128>,
    best_ask: Option<u128>,
    mid_price: Option<u128>,
    last_trade: Option<u128>,
    tick_size: Option<u128>,
) -> Option<u128> {
    let reference_price = match reference_type {
        PegReferenceType::BestBid => best_bid?,
        PegReferenceType::BestAsk => best_ask?,
        PegReferenceType::MidPrice => mid_price?,
        PegReferenceType::LastTrade => last_trade?,
    };

    // Apply offset (can be positive or negative)
    let mut new_price = if offset >= 0 {
        reference_price.saturating_add(offset as u128)
    } else {
        reference_price.saturating_sub((-offset) as u128)
    };
    // The raw target the user requested (`reference ± offset`), before any
    // passive-side clamp / tick snap. Used only for the price-sliding telemetry
    // below — the returned value is unaffected (#174).
    let raw_target = new_price;

    // Effective tick step (1 when unset / <= 1 preserves non-tick-book behavior).
    let step = tick_size.filter(|t| *t > 1).unwrap_or(1);

    // Clamp to the passive side, one *tick* inside the touch so the bound is tick-aligned.
    match side {
        Side::Buy => {
            if let Some(ask) = best_ask {
                new_price = new_price.min(ask.saturating_sub(step));
            }
        }
        Side::Sell => {
            if let Some(bid) = best_bid {
                new_price = new_price.max(bid.saturating_add(step));
            }
        }
    }

    // Snap onto the tick grid in the passive direction. Off-tick prices are rejected
    // by validate_order_shape on re-insert, which would silently abort the re-price.
    if step > 1 {
        new_price = match side {
            Side::Buy => (new_price / step) * step, // round down = more passive
            Side::Sell => new_price.div_ceil(step).saturating_mul(step), // round up = more passive
        };
    }

    // Floor to the minimum valid (tick-aligned) resting price.
    let min_price = if step > 1 { step } else { 1 };
    let priced = new_price.max(min_price);

    // Final non-cross guard: if even the floored price would still cross the touch,
    // there is no valid passive price this cycle — skip the re-price (None) rather than
    // cross or rest off-book. (Fixes the best_ask==1 / ask==step degenerate case.)
    match side {
        Side::Buy => {
            if best_ask.is_some_and(|a| priced >= a) {
                trace!(
                    "pegged re-price skipped (Buy): no valid passive tick below best_ask; raw target {raw_target} would cross"
                );
                return None;
            }
        }
        Side::Sell => {
            if best_bid.is_some_and(|b| priced <= b) {
                trace!(
                    "pegged re-price skipped (Sell): no valid passive tick above best_bid; raw target {raw_target} would cross"
                );
                return None;
            }
        }
    }

    // Price-sliding telemetry (#174): signal when the order was clamped/snapped
    // off its requested `reference ± offset` to the passive side, so a consumer
    // can distinguish a peg that tracked its reference from one that was
    // price-slid to avoid a cross. Does not affect the returned value.
    if priced != raw_target {
        trace!(
            "pegged price clamped to passive side ({side:?}): requested reference+offset {raw_target} -> resting at {priced}"
        );
    }
    Some(priced)
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
            None, // tick_size
        );
        // Raw computed price is 100 + 5 = 105, which equals best_ask and would
        // cross for a Buy. The passive-side clamp caps it at best_ask - 1 = 104
        // so the re-priced order rests just inside the spread instead of trading.
        assert_eq!(price, Some(104));
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
            None, // tick_size
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
            None, // tick_size
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
            None, // tick_size
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
            None, // tick_size
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
            None, // tick_size
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

    /// Computes the deterministic order the tracker is expected to return:
    /// sorted by the `Display`/`to_string` key (same key the tracker uses).
    fn expected_sorted(ids: &[Id]) -> Vec<Id> {
        let mut sorted = ids.to_vec();
        sorted.sort_by_key(|id| id.to_string());
        sorted
    }

    #[test]
    fn test_pegged_order_ids_deterministic_order_issue_106() {
        // Use sequential ids so to_string() order is unambiguous and the
        // registration order deliberately differs from the sorted order.
        let id10 = Id::sequential(10);
        let id2 = Id::sequential(2);
        let id33 = Id::sequential(33);
        let id1 = Id::sequential(1);

        let tracker = SpecialOrderTracker::new();
        // Register in a non-sorted order.
        tracker.register_pegged_order(id10);
        tracker.register_pegged_order(id2);
        tracker.register_pegged_order(id33);
        tracker.register_pegged_order(id1);

        let expected = expected_sorted(&[id10, id2, id33, id1]);
        let got = tracker.pegged_order_ids();
        assert_eq!(got, expected, "pegged_order_ids must be to_string-sorted");

        // Stable across repeated calls.
        assert_eq!(tracker.pegged_order_ids(), got);

        // Order does not depend on insertion order: a fresh tracker built with a
        // different insertion sequence yields the same result.
        let tracker2 = SpecialOrderTracker::new();
        tracker2.register_pegged_order(id1);
        tracker2.register_pegged_order(id33);
        tracker2.register_pegged_order(id2);
        tracker2.register_pegged_order(id10);
        assert_eq!(tracker2.pegged_order_ids(), expected);
    }

    #[test]
    fn test_trailing_stop_ids_deterministic_order_issue_106() {
        let id10 = Id::sequential(10);
        let id2 = Id::sequential(2);
        let id33 = Id::sequential(33);
        let id1 = Id::sequential(1);

        let tracker = SpecialOrderTracker::new();
        tracker.register_trailing_stop(id10);
        tracker.register_trailing_stop(id2);
        tracker.register_trailing_stop(id33);
        tracker.register_trailing_stop(id1);

        let expected = expected_sorted(&[id10, id2, id33, id1]);
        let got = tracker.trailing_stop_ids();
        assert_eq!(got, expected, "trailing_stop_ids must be to_string-sorted");
        assert_eq!(tracker.trailing_stop_ids(), got);

        let tracker2 = SpecialOrderTracker::new();
        tracker2.register_trailing_stop(id1);
        tracker2.register_trailing_stop(id33);
        tracker2.register_trailing_stop(id2);
        tracker2.register_trailing_stop(id10);
        assert_eq!(tracker2.trailing_stop_ids(), expected);
    }

    #[test]
    fn test_calculate_pegged_price_buy_clamped_below_best_ask_issue_106() {
        // Buy peg whose reference + offset lands above the best ask must be
        // capped strictly below the best ask so it never crosses.
        let best_bid = Some(100_u128);
        let best_ask = Some(105_u128);

        // BestBid + 20 = 120, well above the ask -> clamp to best_ask - 1 = 104.
        let price = calculate_pegged_price(
            PegReferenceType::BestBid,
            20,
            Side::Buy,
            best_bid,
            best_ask,
            Some(102),
            Some(101),
            None, // tick_size
        );
        assert_eq!(price, Some(104));
        // Strictly below the best ask (105) so it cannot cross.
        assert!(price < best_ask, "must rest below best ask");
    }

    #[test]
    fn test_calculate_pegged_price_sell_clamped_above_best_bid_issue_106() {
        // Sell peg whose reference - offset lands below the best bid must be
        // floored strictly above the best bid so it never crosses.
        let best_bid = Some(100_u128);
        let best_ask = Some(105_u128);

        // BestAsk - 20 = 85, below the bid -> clamp to best_bid + 1 = 101.
        let price = calculate_pegged_price(
            PegReferenceType::BestAsk,
            -20,
            Side::Sell,
            best_bid,
            best_ask,
            Some(102),
            Some(101),
            None, // tick_size
        );
        assert_eq!(price, Some(101));
        // Strictly above the best bid (100) so it cannot cross.
        assert!(price > best_bid, "must rest above best bid");
    }

    #[test]
    fn test_calculate_pegged_price_no_clamp_when_passive_issue_106() {
        // When the computed price already rests passively, the clamp is a no-op.
        // Buy at 102 with ask 105 -> stays 102.
        let price = calculate_pegged_price(
            PegReferenceType::BestBid,
            2,
            Side::Buy,
            Some(100),
            Some(105),
            Some(102),
            Some(101),
            None, // tick_size
        );
        assert_eq!(price, Some(102));

        // Sell at 104 with bid 100 -> stays 104.
        let price = calculate_pegged_price(
            PegReferenceType::BestAsk,
            -1,
            Side::Sell,
            Some(100),
            Some(105),
            Some(102),
            Some(101),
            None, // tick_size
        );
        assert_eq!(price, Some(104));
    }

    #[test]
    fn test_calculate_pegged_price_buy_tick_aware_clamp_snaps_to_ask_step_issue_106() {
        // tick_size = 5, best_ask = 105, raw = 107 (BestBid 100 + 7) crosses the
        // ask. Clamp to ask - step = 100 (already tick-aligned, strictly below
        // ask). Without the tick-aware bound this would be best_ask - 1 = 104,
        // which is OFF-TICK and silently rejected on re-insert.
        let price = calculate_pegged_price(
            PegReferenceType::BestBid,
            7,
            Side::Buy,
            Some(100),
            Some(105),
            Some(102),
            Some(101),
            Some(5),
        );
        assert_eq!(price, Some(100));
        let p = price.expect("price present");
        assert!(p.is_multiple_of(5), "must be tick-aligned");
        assert!(p < 105, "must rest below best ask");
    }

    #[test]
    fn test_calculate_pegged_price_buy_tick_aware_snaps_offtick_raw_down_issue_106() {
        // tick_size = 5, best_ask = 105, raw = 97 (BestBid 100 - 3) is passive
        // but off-tick. It must snap DOWN (passive direction) to 95.
        let price = calculate_pegged_price(
            PegReferenceType::BestBid,
            -3,
            Side::Buy,
            Some(100),
            Some(105),
            Some(102),
            Some(101),
            Some(5),
        );
        assert_eq!(price, Some(95));
        assert!(price.expect("price present").is_multiple_of(5));
    }

    #[test]
    fn test_calculate_pegged_price_sell_tick_aware_clamp_snaps_to_bid_step_issue_106() {
        // tick_size = 5, best_bid = 100, raw = 112 (BestAsk 105 + 7) would not
        // cross (it's above the bid) but is off-tick; passive snap rounds UP to
        // 115. The bid+step floor (105) does not bind here.
        let price = calculate_pegged_price(
            PegReferenceType::BestAsk,
            7,
            Side::Sell,
            Some(100),
            Some(105),
            Some(102),
            Some(101),
            Some(5),
        );
        assert_eq!(price, Some(115));
        let p = price.expect("price present");
        assert!(p.is_multiple_of(5), "must be tick-aligned");
        assert!(p > 100, "must rest above best bid");
    }

    #[test]
    fn test_calculate_pegged_price_sell_tick_aware_clamp_below_bid_snaps_up_issue_106() {
        // tick_size = 5, best_bid = 100, best_ask = 120, raw = 85 (BestAsk 120
        // - 35) is below the bid. Clamp to bid + step = 105 (tick-aligned, above
        // bid).
        let price = calculate_pegged_price(
            PegReferenceType::BestAsk,
            -35,
            Side::Sell,
            Some(100),
            Some(120),
            Some(110),
            Some(101),
            Some(5),
        );
        assert_eq!(price, Some(105));
        let p = price.expect("price present");
        assert!(p.is_multiple_of(5));
        assert!(p > 100, "must rest above best bid");
    }

    #[test]
    fn test_calculate_pegged_price_buy_no_passive_tick_returns_none_issue_106() {
        // Degenerate: best_ask == step. There is no tick-aligned price strictly
        // below the ask (ask - step = 0, floored to step = 5 == ask, still
        // crosses), so the re-price must be skipped (None) rather than crossing.
        let price = calculate_pegged_price(
            PegReferenceType::BestBid,
            10,
            Side::Buy,
            Some(3),
            Some(5),
            Some(4),
            None,
            Some(5),
        );
        assert_eq!(price, None);

        // Degenerate non-tick book: best_ask == 1. Floor 1 with ask 1 crosses
        // (buy at 1 >= ask 1), so no valid passive price -> None.
        let price = calculate_pegged_price(
            PegReferenceType::BestAsk,
            0,
            Side::Buy,
            None,
            Some(1),
            None,
            None,
            None, // tick_size
        );
        assert_eq!(price, None);
    }

    #[test]
    fn test_calculate_pegged_price_sell_floor_stays_passive_issue_106() {
        // Sell counterpart to the degenerate Buy case: the bid+step clamp always
        // lifts the price strictly above the bid, so a Sell can find a passive
        // resting price even in a 1-wide market. best_bid == 1 on a non-tick
        // book: clamp to bid + 1 = 2, which is above the bid and does not cross.
        let price = calculate_pegged_price(
            PegReferenceType::BestBid,
            -10,
            Side::Sell,
            Some(1),
            None,
            None,
            None,
            None, // tick_size
        );
        assert_eq!(price, Some(2));
    }

    #[test]
    fn test_calculate_pegged_price_sell_no_passive_tick_returns_none_issue_106() {
        // The Sell-side non-cross guard is load-bearing, not dead code: when
        // best_bid is at the top of the range there is no representable tick
        // strictly above it, so bid + step saturates to bid and the guard must
        // skip the re-price (None) rather than rest at/through the bid. This test
        // pins that path so a future "the Sell None branch is unreachable"
        // simplification cannot delete the guard.
        let price = calculate_pegged_price(
            PegReferenceType::BestBid,
            10,
            Side::Sell,
            Some(u128::MAX),
            None,
            None,
            None,
            None, // tick_size
        );
        assert_eq!(price, None);
    }
}
