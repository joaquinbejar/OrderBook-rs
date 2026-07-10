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
use super::order_state::{CancelReason, OrderStatus};
use pricelevel::{Hash32, Id, OrderType, Side, TimestampMs};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// let id1 = Id::new_uuid();
    /// let id2 = Id::new_uuid();
    /// book.add_limit_order(id1, 100, 10, Side::Buy, TimeInForce::Gtc, None)?;
    /// book.add_limit_order(id2, 110, 5, Side::Sell, TimeInForce::Gtc, None)?;
    ///
    /// let result = book.cancel_all_orders();
    /// assert_eq!(result.cancelled_count(), 2);
    /// assert_eq!(book.best_bid(), None);
    /// assert_eq!(book.best_ask(), None);
    /// # Ok(())
    /// # }
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
                let engine_seq = self.next_engine_seq();
                listener(PriceLevelChangedEvent {
                    side: Side::Buy,
                    price: *entry.key(),
                    quantity: 0,
                    engine_seq,
                });
            }
            for entry in self.asks.iter() {
                let engine_seq = self.next_engine_seq();
                listener(PriceLevelChangedEvent {
                    side: Side::Sell,
                    price: *entry.key(),
                    quantity: 0,
                    engine_seq,
                });
            }
        }

        // 2b. Track cancellation state for each order
        for &order_id in &cancelled_order_ids {
            let prev_filled = self
                .order_state_tracker
                .as_ref()
                .and_then(|t| t.get(order_id))
                .map(|s| s.filled_quantity())
                .unwrap_or(0);
            self.track_state(
                order_id,
                OrderStatus::Cancelled {
                    filled_quantity: prev_filled,
                    reason: CancelReason::MassCancelAll,
                },
            );
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

        // 6. Reset the pre-trade risk state. cancel_all empties the whole book, so
        // the per-order on_cancel accounting collapses to a single clear — otherwise
        // every account's open_orders / notional counters would stay at pre-cancel
        // values and permanently reject new flow (#99). No-op without a RiskConfig.
        self.risk_state.clear();

        self.cache.invalidate();
        // Refresh the depth gauges; both sides are now empty.
        self.record_depth_metric();

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
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)?;
    /// book.add_limit_order(Id::new_uuid(), 110, 5, Side::Sell, TimeInForce::Gtc, None)?;
    ///
    /// let result = book.cancel_orders_by_side(Side::Buy);
    /// assert_eq!(result.cancelled_count(), 1);
    /// assert_eq!(book.best_bid(), None);
    /// assert!(book.best_ask().is_some());
    /// # Ok(())
    /// # }
    /// ```
    pub fn cancel_orders_by_side(&self, side: Side) -> MassCancelResult {
        trace!(
            "Order book {}: Mass cancel orders on side {}",
            self.symbol, side
        );

        let order_ids = self.collect_order_ids_by_side(side);
        self.cancel_order_batch_with_reason(&order_ids, CancelReason::MassCancelBySide)
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
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// let user_a = Hash32::new([1u8; 32]);
    /// let user_b = Hash32::new([2u8; 32]);
    ///
    /// book.add_limit_order_with_user(
    ///     Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, user_a, None,
    /// )?;
    /// book.add_limit_order_with_user(
    ///     Id::new_uuid(), 110, 5, Side::Sell, TimeInForce::Gtc, user_b, None,
    /// )?;
    ///
    /// let result = book.cancel_orders_by_user(user_a);
    /// assert_eq!(result.cancelled_count(), 1);
    /// # Ok(())
    /// # }
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

        self.cancel_order_batch_with_reason(&order_ids, CancelReason::MassCancelByUser)
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
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)?;
    /// book.add_limit_order(Id::new_uuid(), 200, 10, Side::Buy, TimeInForce::Gtc, None)?;
    /// book.add_limit_order(Id::new_uuid(), 300, 10, Side::Buy, TimeInForce::Gtc, None)?;
    ///
    /// let result = book.cancel_orders_by_price_range(Side::Buy, 100, 200);
    /// assert_eq!(result.cancelled_count(), 2);
    /// assert_eq!(book.best_bid(), Some(300));
    /// # Ok(())
    /// # }
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

        self.cancel_order_batch_with_reason(&order_ids, CancelReason::MassCancelByPriceRange)
    }

    /// Evict every resting order whose time-in-force has expired at `now_ms`.
    ///
    /// Resting `Gtd` and `Day` orders are only checked for expiry at
    /// *admission* (via `validate_order_shape`); once resting they are never
    /// re-examined by the matching hot path. This is the explicit sweep that
    /// removes them after their deadline. It is **not** invoked automatically —
    /// call it from a scheduler, a per-tick pass, or the sequencer so the
    /// timestamp is journalled and replay stays deterministic.
    ///
    /// # Timestamp
    ///
    /// `now_ms` is **caller-supplied Unix milliseconds** — the same unit as a
    /// `Gtd` deadline and the market-close timestamp. It is taken as an
    /// argument (this method never reads the book's own clock) precisely so the
    /// sequencer can journal the exact instant and reproduce the eviction
    /// byte-for-byte on replay. Boundary behaviour matches admission's
    /// `has_expired`: an order is expired when `now_ms >= deadline` (`Gtd`) or
    /// `now_ms >= market_close` (`Day`); a `Gtd` whose deadline equals `now_ms`
    /// is evicted. `Gtc`, `Ioc`, and `Fok` resting orders are never touched.
    ///
    /// # Determinism contract
    ///
    /// The returned vector — and the [`PriceLevelChangedEvent`] and
    /// `Cancelled { reason: TimeInForceExpired }` state transitions emitted as a
    /// side effect — follow one fixed, replay-stable order:
    ///
    /// 1. **Bids first, then asks.**
    /// 2. Within a side, price levels in **ascending price** order (the
    ///    `SkipMap`'s natural key order — no sorting required).
    /// 3. Within a price level, orders in **ascending insertion sequence** —
    ///    the exact order the matching engine consumes resting orders
    ///    (`PriceLevel::snapshot_by_seq_into`), i.e. oldest first. Note
    ///    this is stable regardless of client-supplied timestamps, unlike the
    ///    non-deterministic `iter_orders` view.
    ///
    /// Every evicted order is removed through the same single-order cancel path
    /// as [`Self::cancel_order`], so the price-level cache, depth statistics,
    /// `order_locations` / `user_orders` indices, risk state, special-order
    /// tracker, and the order-state tracker all stay consistent. Each removal is
    /// tagged with [`CancelReason::TimeInForceExpired`].
    ///
    /// # Idempotence
    ///
    /// A second sweep at the same `now_ms` returns an empty vector: the expired
    /// orders are already gone.
    ///
    /// # Returns
    ///
    /// The evicted orders as `Arc<OrderType<T>>`, in the deterministic order
    /// above. Empty when nothing was expired.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::{Clock, OrderBook, StubClock};
    /// use pricelevel::{Id, Side, TimeInForce, TimestampMs};
    /// use std::sync::Arc;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// // A logical clock starting at 0 so the small GTD deadline is admitted
    /// // (wall-clock admission would treat it as already expired).
    /// let book: OrderBook<()> =
    ///     OrderBook::with_clock("TEST", Arc::new(StubClock::starting_at(0)) as Arc<dyn Clock>);
    /// let gtd = Id::new_uuid();
    /// // A resting GTD order that expires at t = 1_000 ms.
    /// book.add_limit_order(gtd, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)?;
    ///
    /// // Nothing expired yet at t = 999.
    /// assert!(book.evict_expired_orders(TimestampMs::new(999)).is_empty());
    ///
    /// // At the deadline the order is evicted and no longer rests.
    /// let evicted = book.evict_expired_orders(TimestampMs::new(1_000));
    /// assert_eq!(evicted.len(), 1);
    /// assert_eq!(book.best_bid(), None);
    ///
    /// // Idempotent: a second sweep at the same instant evicts nothing.
    /// assert!(book.evict_expired_orders(TimestampMs::new(1_000)).is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub fn evict_expired_orders(&self, now_ms: TimestampMs) -> Vec<Arc<OrderType<T>>> {
        let now = now_ms.as_u64();
        trace!(
            "Order book {}: Evicting expired orders as of {} ms",
            self.symbol, now
        );

        // Phase 1: collect the IDs of every expired resting order in the fixed
        // determinism-contract order (bids ascending, then asks ascending;
        // within each level, ascending insertion sequence). `SkipMap::iter`
        // yields ascending price keys, and `snapshot_by_seq_into` yields the
        // exact order the matching engine consumes resting orders — the
        // non-deterministic `iter_orders` view must NOT be used here or replay
        // would diverge. One scratch buffer is reused across levels to avoid a
        // per-level allocation. Expiry uses `tif_expired_at` — the same
        // definition admission uses — so the boundary case (deadline == now)
        // can never diverge.
        let mut expired_ids: Vec<Id> = Vec::new();
        let mut level_orders: Vec<Arc<OrderType<()>>> = Vec::new();
        for entry in self.bids.iter() {
            entry.value().snapshot_by_seq_into(&mut level_orders);
            for order in &level_orders {
                if self.tif_expired_at(order.time_in_force(), now) {
                    expired_ids.push(order.id());
                }
            }
        }
        for entry in self.asks.iter() {
            entry.value().snapshot_by_seq_into(&mut level_orders);
            for order in &level_orders {
                if self.tif_expired_at(order.time_in_force(), now) {
                    expired_ids.push(order.id());
                }
            }
        }

        if expired_ids.is_empty() {
            return Vec::new();
        }

        // Phase 2: cancel each expired order through the shared single-order
        // path, preserving the collection order. This is what keeps the caches,
        // trackers, and emitted events consistent and in the documented order.
        let mut evicted = Vec::with_capacity(expired_ids.len());
        for order_id in expired_ids {
            if let Ok(Some(order)) =
                self.cancel_order_with_reason(order_id, CancelReason::TimeInForceExpired)
            {
                evicted.push(order);
            }
        }

        trace!(
            symbol = %self.symbol,
            now_ms = now,
            evicted = evicted.len(),
            "expired orders evicted"
        );

        evicted
    }

    /// Internal helper: cancel a batch of orders by their IDs with a reason.
    ///
    /// Calls [`Self::cancel_order_with_reason`] for each ID. Orders that no
    /// longer exist (e.g. concurrently cancelled) are silently skipped.
    fn cancel_order_batch_with_reason(
        &self,
        order_ids: &[Id],
        reason: CancelReason,
    ) -> MassCancelResult {
        let mut cancelled_ids = Vec::with_capacity(order_ids.len());

        for &order_id in order_ids {
            // cancel_order_with_reason handles: listener notification, special order cleanup,
            // empty level removal, order_locations cleanup, and state tracking.
            if let Ok(Some(_)) = self.cancel_order_with_reason(order_id, reason) {
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

    // ---- evict_expired_orders --------------------------------------------

    use crate::orderbook::clock::StubClock;

    /// A book whose clock starts at logical `0`, so small `Gtd` deadlines are
    /// admitted (not seen as already-expired by wall-clock admission) and the
    /// caller-supplied eviction timestamp is what drives expiry.
    fn expiring_book() -> OrderBook<()> {
        OrderBook::with_clock("TEST", Arc::new(StubClock::starting_at(0)))
    }

    #[test]
    fn test_evict_expired_empty_book_returns_empty() {
        let book = expiring_book();
        assert!(
            book.evict_expired_orders(TimestampMs::new(10_000))
                .is_empty()
        );
    }

    #[test]
    fn test_evict_expired_gtd_removed_and_unmatchable() {
        let book = expiring_book();
        let gtd = Id::new_uuid();
        book.add_limit_order(gtd, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("add gtd");

        // Before the deadline: untouched.
        assert!(book.evict_expired_orders(TimestampMs::new(999)).is_empty());
        assert_eq!(book.best_bid(), Some(100));

        // At the deadline (>=): evicted.
        let evicted = book.evict_expired_orders(TimestampMs::new(1_000));
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id(), gtd);
        assert_eq!(book.best_bid(), None);
        assert!(!book.order_locations.contains_key(&gtd));

        // No longer matchable: a crossing sell finds no liquidity.
        let taker = Id::new_uuid();
        assert!(book.match_market_order(taker, 10, Side::Sell).is_err());
    }

    #[test]
    fn test_evict_expired_leaves_gtc_and_unexpired_gtd_untouched() {
        let book = expiring_book();
        let gtc = Id::new_uuid();
        let gtd_future = Id::new_uuid();
        let gtd_past = Id::new_uuid();
        book.add_limit_order(gtc, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("gtc");
        book.add_limit_order(gtd_future, 99, 5, Side::Buy, TimeInForce::Gtd(5_000), None)
            .expect("gtd future");
        book.add_limit_order(gtd_past, 98, 5, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("gtd past");

        let evicted = book.evict_expired_orders(TimestampMs::new(2_000));
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id(), gtd_past);

        assert!(book.order_locations.contains_key(&gtc));
        assert!(book.order_locations.contains_key(&gtd_future));
        assert!(!book.order_locations.contains_key(&gtd_past));
    }

    #[test]
    fn test_evict_expired_boundary_matches_has_expired_semantics() {
        // is_expired is `now >= deadline`; has_expired delegates to the same
        // definition the sweep uses, so `deadline - 1` is not expired and
        // `deadline` is. The sweep must honour that exact boundary.
        let book = expiring_book();
        let id = Id::new_uuid();
        book.add_limit_order(id, 100, 10, Side::Sell, TimeInForce::Gtd(1_000), None)
            .expect("add");

        // 999 -> not expired.
        assert!(book.evict_expired_orders(TimestampMs::new(999)).is_empty());
        // Exactly at the deadline -> evicted.
        let evicted = book.evict_expired_orders(TimestampMs::new(1_000));
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id(), id);
    }

    #[test]
    fn test_evict_expired_day_uses_market_close() {
        let book = expiring_book();
        book.set_market_close_timestamp(2_000);
        let day = Id::new_uuid();
        book.add_limit_order(day, 100, 10, Side::Buy, TimeInForce::Day, None)
            .expect("day");

        // Before close: untouched.
        assert!(
            book.evict_expired_orders(TimestampMs::new(1_999))
                .is_empty()
        );
        // At/after close: evicted.
        let evicted = book.evict_expired_orders(TimestampMs::new(2_000));
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].id(), day);
    }

    #[test]
    fn test_evict_expired_deterministic_order_multiple_levels_sides() {
        let book = expiring_book();

        // Bids at two levels (ascending: 90 then 95), FIFO within each level.
        let b95a = Id::new_uuid();
        let b95b = Id::new_uuid();
        let b90 = Id::new_uuid();
        book.add_limit_order(b95a, 95, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("b95a");
        book.add_limit_order(b95b, 95, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("b95b");
        book.add_limit_order(b90, 90, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("b90");

        // Asks at two levels (ascending: 100 then 110).
        let a100 = Id::new_uuid();
        let a110 = Id::new_uuid();
        book.add_limit_order(a100, 100, 1, Side::Sell, TimeInForce::Gtd(1_000), None)
            .expect("a100");
        book.add_limit_order(a110, 110, 1, Side::Sell, TimeInForce::Gtd(1_000), None)
            .expect("a110");

        let evicted = book.evict_expired_orders(TimestampMs::new(2_000));
        let ids: Vec<Id> = evicted.iter().map(|o| o.id()).collect();

        // Contract: bids ascending (90, then 95 FIFO), then asks ascending.
        assert_eq!(ids, vec![b90, b95a, b95b, a100, a110]);
    }

    #[test]
    fn test_evict_expired_second_sweep_is_idempotent() {
        let book = expiring_book();
        let id = Id::new_uuid();
        book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("add");

        let first = book.evict_expired_orders(TimestampMs::new(1_000));
        assert_eq!(first.len(), 1);
        let second = book.evict_expired_orders(TimestampMs::new(1_000));
        assert!(second.is_empty());
    }

    #[test]
    fn test_evict_expired_fires_book_change_events() {
        use crate::orderbook::book_change_event::PriceLevelChangedEvent;
        use std::sync::Mutex;

        let events: Arc<Mutex<Vec<PriceLevelChangedEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let mut book = expiring_book();
        book.set_price_level_listener(Arc::new(move |ev: PriceLevelChangedEvent| {
            if let Ok(mut v) = sink.lock() {
                v.push(ev);
            }
        }));

        let id = Id::new_uuid();
        book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("add");

        let evicted = book.evict_expired_orders(TimestampMs::new(1_000));
        assert_eq!(evicted.len(), 1);

        let recorded = events.lock().expect("lock");
        // The cancel path emits a level-change event for the touched level.
        assert!(
            recorded
                .iter()
                .any(|ev| ev.side == Side::Buy && ev.price == 100 && ev.quantity == 0)
        );
    }

    #[test]
    fn test_evict_expired_records_time_in_force_expired_reason() {
        use crate::orderbook::order_state::{OrderStateTracker, OrderStatus};

        let mut book = expiring_book();
        book.set_order_state_tracker(OrderStateTracker::new());

        let id = Id::new_uuid();
        book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("add");

        let evicted = book.evict_expired_orders(TimestampMs::new(1_000));
        assert_eq!(evicted.len(), 1);

        let status = book
            .order_state_tracker()
            .and_then(|t| t.get(id))
            .expect("status");
        assert!(matches!(
            status,
            OrderStatus::Cancelled {
                reason: CancelReason::TimeInForceExpired,
                ..
            }
        ));
    }
}
