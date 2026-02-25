//! Mass cancel operations for bulk order removal.
//!
//! Provides efficient methods to cancel multiple orders at once based on
//! various criteria: all orders, by side, by user ID, or by price range.
//! These are critical exchange operations for risk management, market maker
//! position unwinding, and administrative actions.
//!
//! All mass cancel methods reuse the single-order `cancel_order` path,
//! ensuring consistent listener notifications, special-order tracker cleanup,
//! and empty price-level removal.

use super::book::OrderBook;
use super::book_change_event::PriceLevelChangedEvent;
use pricelevel::{Hash32, Id, Side};
use serde::{Deserialize, Serialize};
use tracing::trace;

/// Result of a mass cancel operation.
///
/// Contains the count and identifiers of all orders that were successfully
/// cancelled. This struct is returned by every mass cancel method and should
/// always be inspected by the caller.
///
/// Fields are intentionally private to prevent external mutation of what
/// should be an immutable result type. Use the accessor methods instead.
///
/// # Examples
///
/// ```
/// use orderbook_rs::orderbook::mass_cancel::MassCancelResult;
///
/// let result = MassCancelResult::default();
/// assert_eq!(result.cancelled_count(), 0);
/// assert!(result.cancelled_order_ids().is_empty());
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[must_use]
pub struct MassCancelResult {
    /// Number of orders successfully cancelled.
    cancelled_count: usize,
    /// IDs of all cancelled orders, in the order they were processed.
    cancelled_order_ids: Vec<Id>,
}

impl MassCancelResult {
    /// Creates a new `MassCancelResult` with the given count and order IDs.
    pub(crate) fn new(cancelled_count: usize, cancelled_order_ids: Vec<Id>) -> Self {
        Self {
            cancelled_count,
            cancelled_order_ids,
        }
    }

    /// Returns the number of orders successfully cancelled.
    #[must_use]
    #[inline]
    pub fn cancelled_count(&self) -> usize {
        self.cancelled_count
    }

    /// Returns a slice of all cancelled order IDs, in processing order.
    #[must_use]
    #[inline]
    pub fn cancelled_order_ids(&self) -> &[Id] {
        &self.cancelled_order_ids
    }

    /// Returns `true` if no orders were cancelled.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cancelled_count == 0
    }
}

impl std::fmt::Display for MassCancelResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MassCancelResult {{ cancelled: {} }}",
            self.cancelled_count
        )
    }
}

impl<T> OrderBook<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Cancel all resting orders in the book (both bids and asks).
    ///
    /// This is an optimised bulk operation that clears the entire book in one
    /// pass instead of cancelling orders individually. It:
    /// 1. Collects all resting order IDs.
    /// 2. Emits a [`PriceLevelChangedEvent`] (quantity → 0) for every
    ///    affected price level so that external listeners can update.
    /// 3. Clears all internal tracking maps (`order_locations`, `user_orders`)
    ///    and drains both bid/ask SkipMaps.
    /// 4. Cleans up the special-order tracker (pegged / trailing stop).
    ///
    /// # Performance
    ///
    /// O(L + N) where L = price levels and N = total orders, compared to
    /// O(N log L) for the per-order cancellation path.
    ///
    /// # Returns
    ///
    /// A [`MassCancelResult`] with the count and IDs of all cancelled orders.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// let id1 = Id::new_uuid();
    /// let id2 = Id::new_uuid();
    /// book.add_limit_order(id1, 100, 10, Side::Buy, TimeInForce::Gtc, None).unwrap();
    /// book.add_limit_order(id2, 110, 5, Side::Sell, TimeInForce::Gtc, None).unwrap();
    ///
    /// let result = book.cancel_all_orders();
    /// assert_eq!(result.cancelled_count(), 2);
    /// assert_eq!(book.best_bid(), None);
    /// assert_eq!(book.best_ask(), None);
    /// ```
    pub fn cancel_all_orders(&self) -> MassCancelResult {
        self.cache.invalidate();
        trace!("Order book {}: Mass cancel ALL orders (bulk)", self.symbol);

        // 1. Collect all order IDs before clearing
        let cancelled_order_ids: Vec<Id> = self.order_locations.iter().map(|e| *e.key()).collect();
        let cancelled_count = cancelled_order_ids.len();

        if cancelled_count == 0 {
            return MassCancelResult {
                cancelled_count: 0,
                cancelled_order_ids: Vec::new(),
            };
        }

        // 2. Emit PriceLevelChangedEvent (qty → 0) for every affected level
        if let Some(ref listener) = self.price_level_changed_listener {
            for entry in self.bids.iter() {
                listener(PriceLevelChangedEvent {
                    side: Side::Buy,
                    price: *entry.key(),
                    quantity: 0,
                });
            }
            for entry in self.asks.iter() {
                listener(PriceLevelChangedEvent {
                    side: Side::Sell,
                    price: *entry.key(),
                    quantity: 0,
                });
            }
        }

        // 3. Clear tracking maps
        self.order_locations.clear();
        self.user_orders.clear();

        // 4. Drain both SkipMaps
        while self.bids.pop_front().is_some() {}
        while self.asks.pop_front().is_some() {}

        // 5. Clear special order tracker
        #[cfg(feature = "special_orders")]
        self.special_order_tracker.clear();

        self.cache.invalidate();

        MassCancelResult {
            cancelled_count,
            cancelled_order_ids,
        }
    }

    /// Cancel all resting orders on a specific side (bids or asks).
    ///
    /// Iterates through all price levels on the given side, collects the
    /// order IDs, and cancels each one individually.
    ///
    /// # Arguments
    ///
    /// * `side` — The side to cancel: [`Side::Buy`] for all bids,
    ///   [`Side::Sell`] for all asks.
    ///
    /// # Returns
    ///
    /// A [`MassCancelResult`] with the count and IDs of cancelled orders.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None).unwrap();
    /// book.add_limit_order(Id::new_uuid(), 110, 5, Side::Sell, TimeInForce::Gtc, None).unwrap();
    ///
    /// let result = book.cancel_orders_by_side(Side::Buy);
    /// assert_eq!(result.cancelled_count(), 1);
    /// assert_eq!(book.best_bid(), None);
    /// assert!(book.best_ask().is_some());
    /// ```
    pub fn cancel_orders_by_side(&self, side: Side) -> MassCancelResult {
        trace!(
            "Order book {}: Mass cancel orders on side {}",
            self.symbol, side
        );

        let order_ids = self.collect_order_ids_by_side(side);
        self.cancel_order_batch(&order_ids)
    }

    /// Cancel all resting orders belonging to a specific user.
    ///
    /// Scans every price level on both sides and cancels orders whose
    /// `user_id` matches the given value.
    ///
    /// # Arguments
    ///
    /// * `user_id` — The user identifier to match. Orders with this
    ///   `user_id` will be cancelled.
    ///
    /// # Returns
    ///
    /// A [`MassCancelResult`] with the count and IDs of cancelled orders.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Hash32, Id, Side, TimeInForce};
    ///
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// let user_a = Hash32::new([1u8; 32]);
    /// let user_b = Hash32::new([2u8; 32]);
    ///
    /// book.add_limit_order_with_user(
    ///     Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, user_a, None,
    /// ).unwrap();
    /// book.add_limit_order_with_user(
    ///     Id::new_uuid(), 110, 5, Side::Sell, TimeInForce::Gtc, user_b, None,
    /// ).unwrap();
    ///
    /// let result = book.cancel_orders_by_user(user_a);
    /// assert_eq!(result.cancelled_count(), 1);
    /// ```
    pub fn cancel_orders_by_user(&self, user_id: Hash32) -> MassCancelResult {
        trace!(
            "Order book {}: Mass cancel orders for user {}",
            self.symbol, user_id
        );

        // O(1) lookup via the user_orders index — no full book scan needed.
        let order_ids = self
            .user_orders
            .remove(&user_id)
            .map(|(_, ids)| ids)
            .unwrap_or_default();

        self.cancel_order_batch(&order_ids)
    }

    /// Cancel all resting orders on a given side within a price range
    /// (inclusive on both ends).
    ///
    /// Uses the SkipMap's ordered iteration to efficiently find price levels
    /// within `[min_price, max_price]` and cancels every order at those levels.
    ///
    /// If `min_price > max_price`, no orders are cancelled.
    ///
    /// # Arguments
    ///
    /// * `side` — The side to scan ([`Side::Buy`] or [`Side::Sell`]).
    /// * `min_price` — Lower bound of the price range (inclusive).
    /// * `max_price` — Upper bound of the price range (inclusive).
    ///
    /// # Returns
    ///
    /// A [`MassCancelResult`] with the count and IDs of cancelled orders.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None).unwrap();
    /// book.add_limit_order(Id::new_uuid(), 200, 10, Side::Buy, TimeInForce::Gtc, None).unwrap();
    /// book.add_limit_order(Id::new_uuid(), 300, 10, Side::Buy, TimeInForce::Gtc, None).unwrap();
    ///
    /// let result = book.cancel_orders_by_price_range(Side::Buy, 100, 200);
    /// assert_eq!(result.cancelled_count(), 2);
    /// assert_eq!(book.best_bid(), Some(300));
    /// ```
    pub fn cancel_orders_by_price_range(
        &self,
        side: Side,
        min_price: u128,
        max_price: u128,
    ) -> MassCancelResult {
        trace!(
            "Order book {}: Mass cancel orders on side {} in price range [{}, {}]",
            self.symbol, side, min_price, max_price
        );

        if min_price > max_price {
            return MassCancelResult::default();
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        let mut order_ids = Vec::new();

        for entry in price_levels.range(min_price..=max_price) {
            let level = entry.value();
            for order in level.iter_orders() {
                order_ids.push(order.id());
            }
        }

        self.cancel_order_batch(&order_ids)
    }

    /// Internal helper: cancel a batch of orders by their IDs.
    ///
    /// Calls [`Self::cancel_order`] for each ID. Orders that no longer exist
    /// (e.g. concurrently cancelled) are silently skipped.
    fn cancel_order_batch(&self, order_ids: &[Id]) -> MassCancelResult {
        let mut cancelled_ids = Vec::with_capacity(order_ids.len());

        for &order_id in order_ids {
            // cancel_order handles: listener notification, special order cleanup,
            // empty level removal, and order_locations cleanup.
            if let Ok(Some(_)) = self.cancel_order(order_id) {
                cancelled_ids.push(order_id);
            }
        }

        let count = cancelled_ids.len();
        MassCancelResult::new(count, cancelled_ids)
    }

    /// Collect all order IDs on a given side by iterating price levels.
    fn collect_order_ids_by_side(&self, side: Side) -> Vec<Id> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        let mut ids = Vec::new();
        for entry in price_levels.iter() {
            let level = entry.value();
            for order in level.iter_orders() {
                ids.push(order.id());
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::TimeInForce;

    #[test]
    fn test_mass_cancel_result_default() {
        let result = MassCancelResult::default();
        assert_eq!(result.cancelled_count(), 0);
        assert!(result.cancelled_order_ids().is_empty());
        assert!(result.is_empty());
    }

    #[test]
    fn test_mass_cancel_result_display() {
        let result = MassCancelResult::new(5, vec![]);
        assert_eq!(result.to_string(), "MassCancelResult { cancelled: 5 }");
    }

    #[test]
    fn test_cancel_all_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        let result = book.cancel_all_orders();
        assert!(result.is_empty());
        assert_eq!(result.cancelled_count(), 0);
    }

    #[test]
    fn test_cancel_all_with_orders() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let id1 = Id::new_uuid();
        let id2 = Id::new_uuid();
        let id3 = Id::new_uuid();

        book.add_limit_order(id1, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");
        book.add_limit_order(id2, 200, 5, Side::Sell, TimeInForce::Gtc, None)
            .expect("add ask");
        book.add_limit_order(id3, 95, 20, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid 2");

        let result = book.cancel_all_orders();
        assert_eq!(result.cancelled_count(), 3);
        assert_eq!(result.cancelled_order_ids().len(), 3);
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn test_cancel_by_side_buy() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");
        book.add_limit_order(Id::new_uuid(), 95, 5, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid 2");
        book.add_limit_order(Id::new_uuid(), 200, 8, Side::Sell, TimeInForce::Gtc, None)
            .expect("add ask");

        let result = book.cancel_orders_by_side(Side::Buy);
        assert_eq!(result.cancelled_count(), 2);
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), Some(200));
    }

    #[test]
    fn test_cancel_by_side_sell() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");
        book.add_limit_order(Id::new_uuid(), 200, 8, Side::Sell, TimeInForce::Gtc, None)
            .expect("add ask");
        book.add_limit_order(Id::new_uuid(), 210, 3, Side::Sell, TimeInForce::Gtc, None)
            .expect("add ask 2");

        let result = book.cancel_orders_by_side(Side::Sell);
        assert_eq!(result.cancelled_count(), 2);
        assert_eq!(book.best_bid(), Some(100));
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn test_cancel_by_side_empty() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");

        let result = book.cancel_orders_by_side(Side::Sell);
        assert!(result.is_empty());
        assert_eq!(book.best_bid(), Some(100));
    }

    #[test]
    fn test_cancel_by_user() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let user_a = Hash32::new([1u8; 32]);
        let user_b = Hash32::new([2u8; 32]);

        let id_a1 = Id::new_uuid();
        let id_a2 = Id::new_uuid();
        let id_b1 = Id::new_uuid();

        book.add_limit_order_with_user(id_a1, 100, 10, Side::Buy, TimeInForce::Gtc, user_a, None)
            .expect("add a1");
        book.add_limit_order_with_user(id_a2, 200, 5, Side::Sell, TimeInForce::Gtc, user_a, None)
            .expect("add a2");
        book.add_limit_order_with_user(id_b1, 95, 20, Side::Buy, TimeInForce::Gtc, user_b, None)
            .expect("add b1");

        let result = book.cancel_orders_by_user(user_a);
        assert_eq!(result.cancelled_count(), 2);
        assert!(result.cancelled_order_ids().contains(&id_a1));
        assert!(result.cancelled_order_ids().contains(&id_a2));

        // user_b's order remains
        assert_eq!(book.best_bid(), Some(95));
        assert_eq!(book.best_ask(), None);
        assert_eq!(book.order_locations.len(), 1);
    }

    #[test]
    fn test_cancel_by_user_no_match() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let user_a = Hash32::new([1u8; 32]);
        let user_b = Hash32::new([2u8; 32]);

        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            user_a,
            None,
        )
        .expect("add a1");

        let result = book.cancel_orders_by_user(user_b);
        assert!(result.is_empty());
        assert_eq!(book.order_locations.len(), 1);
    }

    #[test]
    fn test_cancel_by_price_range() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let id1 = Id::new_uuid();
        let id2 = Id::new_uuid();
        let id3 = Id::new_uuid();

        book.add_limit_order(id1, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add 100");
        book.add_limit_order(id2, 200, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add 200");
        book.add_limit_order(id3, 300, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add 300");

        let result = book.cancel_orders_by_price_range(Side::Buy, 100, 200);
        assert_eq!(result.cancelled_count(), 2);
        assert!(result.cancelled_order_ids().contains(&id1));
        assert!(result.cancelled_order_ids().contains(&id2));
        assert_eq!(book.best_bid(), Some(300));
    }

    #[test]
    fn test_cancel_by_price_range_inverted() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add");

        // min > max → no cancellation
        let result = book.cancel_orders_by_price_range(Side::Buy, 200, 100);
        assert!(result.is_empty());
        assert_eq!(book.order_locations.len(), 1);
    }

    #[test]
    fn test_cancel_by_price_range_no_match() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add");

        let result = book.cancel_orders_by_price_range(Side::Buy, 200, 300);
        assert!(result.is_empty());
        assert_eq!(book.order_locations.len(), 1);
    }

    #[test]
    fn test_cancel_by_price_range_exact_boundaries() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let id1 = Id::new_uuid();
        let id2 = Id::new_uuid();

        book.add_limit_order(id1, 100, 10, Side::Sell, TimeInForce::Gtc, None)
            .expect("add 100");
        book.add_limit_order(id2, 200, 10, Side::Sell, TimeInForce::Gtc, None)
            .expect("add 200");

        // Exact single price
        let result = book.cancel_orders_by_price_range(Side::Sell, 100, 100);
        assert_eq!(result.cancelled_count(), 1);
        assert!(result.cancelled_order_ids().contains(&id1));
        assert_eq!(book.best_ask(), Some(200));
    }

    #[test]
    fn test_cancel_all_with_iceberg_orders() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let id1 = Id::new_uuid();
        let id2 = Id::new_uuid();

        book.add_iceberg_order(id1, 100, 5, 15, Side::Buy, TimeInForce::Gtc, None)
            .expect("add iceberg");
        book.add_limit_order(id2, 200, 10, Side::Sell, TimeInForce::Gtc, None)
            .expect("add limit");

        let result = book.cancel_all_orders();
        assert_eq!(result.cancelled_count(), 2);
        assert!(book.order_locations.is_empty());
    }

    #[test]
    fn test_cancel_all_with_post_only_orders() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let id1 = Id::new_uuid();
        let id2 = Id::new_uuid();

        book.add_post_only_order(id1, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add post-only");
        book.add_limit_order(id2, 200, 10, Side::Sell, TimeInForce::Gtc, None)
            .expect("add limit");

        let result = book.cancel_all_orders();
        assert_eq!(result.cancelled_count(), 2);
        assert!(book.order_locations.is_empty());
    }

    #[test]
    fn test_cancel_by_user_multiple_levels() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let user = Hash32::new([1u8; 32]);
        let other = Hash32::new([2u8; 32]);

        // User has orders at multiple price levels on both sides
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            user,
            None,
        )
        .expect("add buy 100");
        book.add_limit_order_with_user(
            Id::new_uuid(),
            95,
            5,
            Side::Buy,
            TimeInForce::Gtc,
            user,
            None,
        )
        .expect("add buy 95");
        book.add_limit_order_with_user(
            Id::new_uuid(),
            200,
            8,
            Side::Sell,
            TimeInForce::Gtc,
            user,
            None,
        )
        .expect("add sell 200");
        book.add_limit_order_with_user(
            Id::new_uuid(),
            90,
            20,
            Side::Buy,
            TimeInForce::Gtc,
            other,
            None,
        )
        .expect("add other buy");

        let result = book.cancel_orders_by_user(user);
        assert_eq!(result.cancelled_count(), 3);
        // Only other user's order remains
        assert_eq!(book.order_locations.len(), 1);
        assert_eq!(book.best_bid(), Some(90));
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn test_cancel_by_price_range_multiple_orders_same_level() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let id1 = Id::new_uuid();
        let id2 = Id::new_uuid();
        let id3 = Id::new_uuid();

        // Two orders at same price level
        book.add_limit_order(id1, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add 1");
        book.add_limit_order(id2, 100, 20, Side::Buy, TimeInForce::Gtc, None)
            .expect("add 2");
        book.add_limit_order(id3, 200, 5, Side::Buy, TimeInForce::Gtc, None)
            .expect("add 3");

        let result = book.cancel_orders_by_price_range(Side::Buy, 100, 100);
        assert_eq!(result.cancelled_count(), 2);
        assert_eq!(book.best_bid(), Some(200));
        assert!(book.bids.get(&100).is_none());
    }

    #[test]
    fn test_order_locations_cleaned_after_mass_cancel() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let id1 = Id::new_uuid();
        let id2 = Id::new_uuid();

        book.add_limit_order(id1, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add 1");
        book.add_limit_order(id2, 200, 5, Side::Sell, TimeInForce::Gtc, None)
            .expect("add 2");

        assert!(book.order_locations.contains_key(&id1));
        assert!(book.order_locations.contains_key(&id2));

        let _ = book.cancel_all_orders();

        assert!(!book.order_locations.contains_key(&id1));
        assert!(!book.order_locations.contains_key(&id2));
        assert!(book.order_locations.is_empty());
    }

    #[test]
    fn test_mass_cancel_result_is_must_use() {
        // This test ensures the struct compiles with #[must_use].
        // The compiler would warn if the result were ignored in real code.
        let book: OrderBook<()> = OrderBook::new("TEST");
        let result = book.cancel_all_orders();
        assert!(result.is_empty());
    }
}
