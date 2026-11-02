//! Mass cancel operations for bulk order removal.
//!
//! Provides efficient methods to cancel multiple orders at once based on
//! various criteria: all orders, by side, by user ID, or by price range.
//! These are critical exchange operations for risk management, market maker
//! position unwinding, and administrative actions.
//!
//! The scoped mass cancels and expiry eviction reuse the single-order
//! `cancel_order` path, ensuring consistent listener notifications, risk
//! release, special-order tracker cleanup, and empty price-level removal;
//! `cancel_all_orders` empties the book in bulk. Every mass cancel holds the
//! exclusive side of the submit gate, and none of them swallows a failure:
//! see [`MassCancelFailure`] (#248).

use super::book::OrderBook;
use super::error::OrderBookError;
use super::order_state::{CancelReason, OrderStatus};
use pricelevel::{Hash32, Id, OrderType, PriceLevel, PriceLevelError, Side, TimestampMs};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::trace;

/// A failure recorded by a mass cancel operation instead of being swallowed.
///
/// Three kinds of failure exist, and they mean different things for the book:
///
/// - [`Self::LevelUnreadable`] is a **refusal**. Every mass cancel that
///   walks price levels (`cancel_all_orders`, `cancel_orders_by_side`,
///   `cancel_orders_by_price_range`) reads each level's resting orders
///   through the fallible `PriceLevel::snapshot_by_seq_into` (pricelevel
///   0.10). The read phase runs before any order is cancelled, and a level
///   that cannot be read makes the whole call cancel **nothing**: a partial
///   bulk cancel whose skipped orders are invisible in the journaled payload
///   would be worse than a refused one. See [`MassCancelResult::is_refused`].
/// - [`Self::OrderCancelFailed`] is a **per-order** failure (#248). The
///   scoped mass cancels remove orders one at a time through the
///   single-order cancel path; an order whose price level refuses the
///   removal stays resting and fully tracked (location, user index, risk,
///   order state), the failure is recorded here, and the call carries on
///   with the next order. Such a result can therefore carry cancelled ids
///   **and** failures.
/// - [`Self::LevelFaultAfterRemoval`] is a per-order **fault report**: the
///   level removed the order and then failed. The order was cancelled, is
///   listed in the cancelled ids, and the book completed its removal.
///
/// Failures are recorded in the call's deterministic traversal order, the
/// same order as [`MassCancelResult::cancelled_order_ids`].
///
/// The enum is `#[non_exhaustive]`: later releases may add variants, so
/// match it with a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MassCancelFailure {
    /// The resting orders of the price level at `price` on `side` could not
    /// be read; the mass cancel was refused and nothing was cancelled.
    LevelUnreadable {
        /// Side of the unreadable level.
        side: Side,
        /// Price of the unreadable level, in price ticks.
        price: u128,
        /// The error pricelevel returned for the read.
        error: PriceLevelError,
    },
    /// The order `order_id` was in the mass cancel's scope but its price
    /// level refused the removal (#248). The order is still resting and
    /// tracked; no cancel event, order-state transition or risk release was
    /// emitted for it.
    OrderCancelFailed {
        /// The order that could not be cancelled.
        order_id: Id,
        /// The error the single-order cancel path returned.
        error: PriceLevelError,
    },
    /// The level removed `order_id` and then reported `error` (#248; see
    /// [`OrderBookError::OrderRemovedWithLevelFault`]). The order **was**
    /// cancelled: it is listed in
    /// [`MassCancelResult::cancelled_order_ids`] and the book completed the
    /// removal. This entry reports that the level is now faulty.
    LevelFaultAfterRemoval {
        /// The order that was removed.
        order_id: Id,
        /// The failure the level reported after the removal.
        error: PriceLevelError,
    },
}

impl MassCancelFailure {
    /// Converts the recorded failure into the equivalent [`OrderBookError`].
    #[must_use]
    pub fn to_order_book_error(&self) -> OrderBookError {
        match self {
            MassCancelFailure::LevelUnreadable { error, .. }
            | MassCancelFailure::OrderCancelFailed { error, .. } => {
                OrderBookError::PriceLevelError(error.clone())
            }
            MassCancelFailure::LevelFaultAfterRemoval { order_id, error } => {
                OrderBookError::OrderRemovedWithLevelFault {
                    order_id: *order_id,
                    source: Box::new(error.clone()),
                }
            }
        }
    }

    /// Returns `true` for a failure that refused the whole call
    /// ([`Self::LevelUnreadable`]): nothing was cancelled.
    #[must_use]
    #[inline]
    pub fn is_refusal(&self) -> bool {
        matches!(self, MassCancelFailure::LevelUnreadable { .. })
    }

    /// Builds the per-order failure for an `order_id` the single-order
    /// cancel path could not remove. The cancel path only fails with
    /// [`OrderBookError::PriceLevelError`]; any other variant is folded into
    /// [`PriceLevelError::InvalidOperation`] carrying its message, so the
    /// failure is still recorded rather than dropped.
    #[cold]
    #[inline(never)]
    pub(crate) fn order_cancel_failed(order_id: Id, error: OrderBookError) -> Self {
        MassCancelFailure::OrderCancelFailed {
            order_id,
            error: cancel_error_source(error),
        }
    }
}

impl std::fmt::Display for MassCancelFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MassCancelFailure::LevelUnreadable { side, price, error } => {
                write!(f, "price level {side} {price} unreadable: {error}")
            }
            MassCancelFailure::OrderCancelFailed { order_id, error } => {
                write!(f, "order {order_id} not cancelled: {error}")
            }
            MassCancelFailure::LevelFaultAfterRemoval { order_id, error } => {
                write!(
                    f,
                    "order {order_id} cancelled but its price level then failed: {error}"
                )
            }
        }
    }
}

/// Result of a mass cancel operation.
///
/// Contains the count and identifiers of all orders that were successfully
/// cancelled, plus every [`MassCancelFailure`] the call recorded. This struct
/// is returned by every mass cancel method and should always be inspected by
/// the caller: check [`Self::has_failures`] before treating the result as
/// complete, and [`Self::is_refused`] to tell a refused call (nothing
/// cancelled) from a partial one.
///
/// # Serialization
///
/// `failures` was added in 0.14.0 with `#[serde(default)]`: JSON written by
/// earlier releases (for example a journaled `MassCancelled` entry) decodes
/// with an empty failure list. The `order_cancel_failed` and
/// `level_fault_after_removal` failure variants are also new in 0.14.0
/// (#248); JSON carrying only `level_unreadable` failures decodes
/// unchanged. Positional encodings (bincode)
/// written by earlier releases do not decode.
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
    /// Failures recorded by the call, in traversal order.
    #[serde(default)]
    failures: Vec<MassCancelFailure>,
}

impl MassCancelResult {
    /// Creates a new `MassCancelResult` with the given count and order IDs.
    pub(crate) fn new(cancelled_count: usize, cancelled_order_ids: Vec<Id>) -> Self {
        Self {
            cancelled_count,
            cancelled_order_ids,
            failures: Vec::new(),
        }
    }

    /// Creates a result from the cancelled ids and the per-order failures of
    /// a call, both in traversal order.
    pub(crate) fn with_failures(
        cancelled_order_ids: Vec<Id>,
        failures: Vec<MassCancelFailure>,
    ) -> Self {
        Self {
            cancelled_count: cancelled_order_ids.len(),
            cancelled_order_ids,
            failures,
        }
    }

    /// Creates a result for a mass cancel refused by `failure`: nothing was
    /// cancelled.
    #[cold]
    #[inline(never)]
    pub(crate) fn refused(failure: MassCancelFailure) -> Self {
        Self {
            cancelled_count: 0,
            cancelled_order_ids: Vec::new(),
            failures: vec![failure],
        }
    }

    /// Returns the failures recorded by the operation, in traversal order.
    ///
    /// Empty when the operation completed its whole scope.
    #[must_use]
    #[inline]
    pub fn failures(&self) -> &[MassCancelFailure] {
        &self.failures
    }

    /// Returns `true` if the operation recorded at least one failure.
    #[must_use]
    #[inline]
    pub fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    /// Returns `true` if the operation was **refused** as a whole
    /// ([`MassCancelFailure::LevelUnreadable`]): nothing was cancelled.
    ///
    /// A result with only [`MassCancelFailure::OrderCancelFailed`] failures
    /// is partial, not refused: the listed ids were cancelled and the failed
    /// orders are still resting.
    #[must_use]
    #[inline]
    pub fn is_refused(&self) -> bool {
        self.failures.iter().any(MassCancelFailure::is_refusal)
    }

    /// Returns the ids of the orders the operation failed to cancel, which
    /// are still resting ([`MassCancelFailure::OrderCancelFailed`]), in
    /// traversal order.
    pub fn failed_order_ids(&self) -> impl Iterator<Item = Id> + '_ {
        self.failures.iter().filter_map(|failure| match failure {
            MassCancelFailure::OrderCancelFailed { order_id, .. } => Some(*order_id),
            _ => None,
        })
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
    ///
    /// An empty result can also be a refused one; see [`Self::has_failures`].
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cancelled_count == 0
    }
}

impl std::fmt::Display for MassCancelResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.failures.is_empty() {
            write!(
                f,
                "MassCancelResult {{ cancelled: {} }}",
                self.cancelled_count
            )
        } else {
            write!(
                f,
                "MassCancelResult {{ cancelled: {}, failures: {} }}",
                self.cancelled_count,
                self.failures.len()
            )
        }
    }
}

/// Result of [`OrderBook::evict_expired_orders`] (#248).
///
/// Carries the evicted orders' bodies and a [`MassCancelResult`] with the
/// evicted ids and every per-order failure, both in the sweep's
/// deterministic order. Journal the eviction with
/// [`Self::mass_cancel_result`] (as `SequencerResult::MassCancelled`) so
/// replay reproduces exactly the orders the live sweep evicted.
///
/// [`Self::evicted_orders`] can be shorter than
/// [`Self::evicted_order_ids`]: an order whose level removed it and then
/// failed ([`MassCancelFailure::LevelFaultAfterRemoval`]) was evicted, but
/// the level returned no body for it.
#[derive(Debug, Clone)]
#[must_use]
pub struct EvictionResult<T> {
    /// Bodies of the evicted orders, in sweep order.
    evicted: Vec<Arc<OrderType<T>>>,
    /// Evicted ids and per-order failures, in sweep order.
    result: MassCancelResult,
}

impl<T> Default for EvictionResult<T> {
    fn default() -> Self {
        Self {
            evicted: Vec::new(),
            result: MassCancelResult::default(),
        }
    }
}

impl<T> EvictionResult<T> {
    /// The evicted orders, in sweep order.
    #[must_use]
    #[inline]
    pub fn evicted_orders(&self) -> &[Arc<OrderType<T>>] {
        &self.evicted
    }

    /// Iterates over the evicted orders, in sweep order.
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, Arc<OrderType<T>>> {
        self.evicted.iter()
    }

    /// Consumes the result, returning the evicted orders.
    #[must_use]
    #[inline]
    pub fn into_evicted_orders(self) -> Vec<Arc<OrderType<T>>> {
        self.evicted
    }

    /// The ids of every evicted order, in sweep order.
    #[must_use]
    #[inline]
    pub fn evicted_order_ids(&self) -> &[Id] {
        self.result.cancelled_order_ids()
    }

    /// Number of evicted orders.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.result.cancelled_count()
    }

    /// Returns `true` if nothing was evicted. A sweep with failures can be
    /// empty too; see [`Self::has_failures`].
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.result.is_empty()
    }

    /// Per-order failures, in sweep order.
    #[must_use]
    #[inline]
    pub fn failures(&self) -> &[MassCancelFailure] {
        self.result.failures()
    }

    /// Returns `true` if the sweep recorded at least one failure.
    #[must_use]
    #[inline]
    pub fn has_failures(&self) -> bool {
        self.result.has_failures()
    }

    /// The eviction as a [`MassCancelResult`] (ids and failures), the shape
    /// to journal as `SequencerResult::MassCancelled`.
    #[inline]
    pub fn mass_cancel_result(&self) -> &MassCancelResult {
        &self.result
    }

    /// Consumes the result, returning the [`MassCancelResult`].
    #[inline]
    pub fn into_mass_cancel_result(self) -> MassCancelResult {
        self.result
    }
}

impl<'a, T> IntoIterator for &'a EvictionResult<T> {
    type Item = &'a Arc<OrderType<T>>;
    type IntoIter = std::slice::Iter<'a, Arc<OrderType<T>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.evicted.iter()
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
    /// 1. Collects all resting order IDs and the affected price levels.
    /// 2. Clears all internal tracking maps (`order_locations`, `user_orders`),
    ///    drains both bid/ask SkipMaps, cleans up the special-order tracker
    ///    (pegged / trailing stop) and releases every order's pre-trade risk
    ///    contribution.
    /// 3. Only then emits a [`PriceLevelChangedEvent`](super::book_change_event::PriceLevelChangedEvent) (quantity → 0) for every
    ///    affected price level and a `Cancelled { MassCancelAll }` order-state
    ///    transition for every cancelled order, so a listener never observes
    ///    an event for a mutation that has not happened yet.
    ///
    /// # Concurrency (#248)
    ///
    /// The whole call holds the **exclusive** side of the submit gate. The
    /// bulk clear removes everything the tracking maps hold, not just what
    /// step 1 collected, so under the shared side an order admitted between
    /// the collection and the clear was dropped with no cancel event, no
    /// order-state transition and no risk release, and the risk reset wiped
    /// the reservations of in-flight submits. Under the exclusive side the
    /// book is quiescent: every order is either cancelled and reported here,
    /// or admitted after the call and still resting and tracked. The cost is
    /// that submits, cancels and modifies on this book wait for the bulk
    /// clear to finish.
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
    /// # Determinism
    ///
    /// [`MassCancelResult::cancelled_order_ids`] — and therefore the journaled
    /// `SequencerResult::MassCancelled` payload — follows one fixed,
    /// replay-stable order, identical to the
    /// [`Self::evict_expired_orders`] contract:
    ///
    /// 1. **Bids first, then asks.**
    /// 2. Within a side, price levels in **ascending price** order (the
    ///    `SkipMap`'s natural key order — no sorting required).
    /// 3. Within a price level, orders in **ascending insertion sequence** —
    ///    the exact order the matching engine consumes resting orders
    ///    (`PriceLevel::snapshot_by_seq_into`), i.e. oldest first.
    ///
    /// This traversal is independent of the `order_locations` / `user_orders`
    /// `DashMap` iteration order (each instance seeds its own randomised
    /// hasher), so the id sequence — and the `SequencerResult::MassCancelled`
    /// event that carries it — is byte-identical across processes and replay.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// let id1 = Id::from_uuid(Uuid::new_v4());
    /// let id2 = Id::from_uuid(Uuid::new_v4());
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
        // #248: exclusive submit gate. The bulk clear below empties the
        // tracking maps wholesale, so no admission, cancel or modify may run
        // between the collection and the clear (see "Concurrency" above).
        let _gate = self.submit_gate_write();
        self.cache.invalidate();
        trace!("Order book {}: Mass cancel ALL orders (bulk)", self.symbol);

        // 1. Collect all order IDs before clearing, in the fixed
        // determinism-contract order (bids ascending price, then asks ascending
        // price; within each level ascending insertion sequence). `SkipMap::iter`
        // yields ascending price keys, and `snapshot_by_seq_into` yields the exact
        // order the matching engine consumes resting orders — the `order_locations`
        // `DashMap` iteration order must NOT be used here or replay would diverge
        // across processes (its hasher is seeded per-instance). One scratch buffer
        // is reused across levels to avoid a per-level allocation. The affected
        // levels are recorded in the same order for the post-clear events.
        //
        // The collection runs before any mutation: a level whose orders
        // cannot be read refuses the whole call (nothing is cancelled and the
        // level is reported in `failures`), because the bulk clear below would
        // otherwise remove orders that neither the result nor the journal names.
        let mut cancelled_order_ids: Vec<Id> = Vec::with_capacity(self.order_locations.len());
        let mut cleared_levels: Vec<(Side, u128)> =
            Vec::with_capacity(self.bids.len().checked_add(self.asks.len()).unwrap_or(0));
        let mut level_orders: Vec<Arc<OrderType<()>>> = Vec::new();
        let sides = self
            .bids
            .iter()
            .map(|entry| (entry, Side::Buy))
            .chain(self.asks.iter().map(|entry| (entry, Side::Sell)));
        for (entry, side) in sides {
            let price = *entry.key();
            if let Err(failure) = read_level_orders(entry.value(), side, price, &mut level_orders) {
                return self.refuse_mass_cancel(failure);
            }
            cleared_levels.push((side, price));
            for order in &level_orders {
                cancelled_order_ids.push(order.id());
            }
        }
        // #286: then every pending trailing stop, in admission order.
        let resting_count = cancelled_order_ids.len();
        cancelled_order_ids.extend(self.stops_in_scope(|_| true));
        let cancelled_count = cancelled_order_ids.len();

        if cancelled_count == 0 {
            return MassCancelResult::default();
        }

        // 2. Mutate. Everything below is infallible, so the book goes from
        // "every collected order resting" to "empty" with nothing in between
        // observable by a listener.
        //
        // 2a. Clear tracking maps. Exclusive gate: the maps hold exactly the
        // collected orders.
        self.order_locations.clear();
        self.user_orders.clear();
        // #230: `cancel_all_orders` is the one removal path that does not go
        // through `cancel_order_with_reason` — it empties the whole book in
        // bulk — so the strandable-maker tally collapses to a single reset
        // here, the same way the risk state does below. Nothing rests
        // afterwards, so the exact count is zero.
        self.reset_strandable_makers();

        // 2b. Drain both SkipMaps
        while self.bids.pop_front().is_some() {}
        while self.asks.pop_front().is_some() {}

        // 2c. Clear special order tracker and the pending trailing stops
        // (#286; their risk contribution goes with the clear below).
        #[cfg(feature = "special_orders")]
        {
            self.special_order_tracker.clear();
            self.pending_stops.clear();
        }

        // 2d. Release the pre-trade risk state. cancel_all empties the whole
        // book, so the per-order on_cancel accounting collapses to a single
        // clear — otherwise every account's open_orders / notional counters
        // would stay at pre-cancel values and permanently reject new flow
        // (#99). Exact only because the exclusive gate excludes in-flight
        // submits, whose risk reservations the clear would otherwise wipe
        // (#248). No-op without a RiskConfig.
        self.risk_state.clear();

        self.cache.invalidate();
        // Refresh the depth gauges; both sides are now empty.
        self.record_depth_metric();

        // 3. Emit, after the mutation: one PriceLevelChangedEvent (qty → 0)
        // per cleared level, then one Cancelled transition per order, both in
        // the collection order (the same level-then-state order the
        // single-order cancel path emits).
        if self.price_level_changed_listener.is_some() {
            for &(side, price) in &cleared_levels {
                self.emit_price_level_changed(side, price, 0);
            }
        }
        for (index, &order_id) in cancelled_order_ids.iter().enumerate() {
            // A pending stop (#286) never filled.
            let prev_filled = if index < resting_count {
                self.order_state_tracker
                    .as_ref()
                    .and_then(|t| t.get(order_id))
                    .map(|s| s.filled_quantity())
                    .unwrap_or(0)
            } else {
                0
            };
            self.track_state(
                order_id,
                OrderStatus::Cancelled {
                    filled_quantity: prev_filled,
                    reason: CancelReason::MassCancelAll,
                },
            );
        }

        MassCancelResult::new(cancelled_count, cancelled_order_ids)
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
    /// An order whose price level refuses the removal is recorded as
    /// [`MassCancelFailure::OrderCancelFailed`] and stays resting and
    /// tracked; the call carries on with the rest of its scope (#248).
    ///
    /// # Concurrency
    ///
    /// Holds the **exclusive** side of the submit gate (#248): the scope is
    /// collected before it is cancelled, and an id re-admitted out of scope
    /// in between must not be cancelled by this call.
    ///
    /// # Determinism
    ///
    /// [`MassCancelResult::cancelled_order_ids`] — and therefore the journaled
    /// `SequencerResult::MassCancelled` payload — follows one fixed,
    /// replay-stable order, matching the [`Self::evict_expired_orders`]
    /// contract restricted to the requested side:
    ///
    /// 1. Price levels in **ascending price** order (the `SkipMap`'s natural
    ///    key order — no sorting required).
    /// 2. Within a price level, orders in **ascending insertion sequence** —
    ///    the exact order the matching engine consumes resting orders
    ///    (`PriceLevel::snapshot_by_seq_into`), i.e. oldest first.
    ///
    /// The traversal never observes the `order_locations` / `user_orders`
    /// `DashMap` iteration order (seeded per instance), so the id sequence is
    /// byte-identical across processes and replay.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None)?;
    /// book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 110, 5, Side::Sell, TimeInForce::Gtc, None)?;
    ///
    /// let result = book.cancel_orders_by_side(Side::Buy);
    /// assert_eq!(result.cancelled_count(), 1);
    /// assert_eq!(book.best_bid(), None);
    /// assert!(book.best_ask().is_some());
    /// # Ok(())
    /// # }
    /// ```
    pub fn cancel_orders_by_side(&self, side: Side) -> MassCancelResult {
        // #248: exclusive submit gate. The scope is collected first and
        // cancelled id by id afterwards; under the shared side an id could be
        // cancelled and re-admitted out of scope in between (other side,
        // other price, other user, not expired) and would then be cancelled
        // by this call. Exclusive also keeps a concurrent FOK / STP window
        // (#209 / #225) out of the walk, as the shared side did.
        let _gate = self.submit_gate_write();
        trace!(
            "Order book {}: Mass cancel orders on side {}",
            self.symbol, side
        );

        match self.collect_order_ids_by_side(side) {
            Ok(mut order_ids) => {
                // #286: then the side's pending stops, in admission order.
                order_ids.extend(self.stops_in_scope(|stop| stop.side() == side));
                self.cancel_order_batch_with_reason(&order_ids, CancelReason::MassCancelBySide)
            }
            Err(failure) => self.refuse_mass_cancel(failure),
        }
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
    /// An order whose price level refuses the removal is recorded as
    /// [`MassCancelFailure::OrderCancelFailed`] and stays resting and
    /// tracked; the call carries on with the rest of its scope (#248).
    ///
    /// # Concurrency
    ///
    /// Holds the **exclusive** side of the submit gate (#248): the scope is
    /// collected before it is cancelled, and an id re-admitted out of scope
    /// in between must not be cancelled by this call.
    ///
    /// # Determinism
    ///
    /// [`MassCancelResult::cancelled_order_ids`] follows the user's
    /// **admission-history order**: the `user_orders` index records each of
    /// the user's ids under an admission sequence as it is admitted
    /// (`track_user_order`); cancels, fills and modifies take ids out
    /// without renumbering the ids that remain (#252), and this method walks
    /// them sorted by that sequence. Under a
    /// serialized command stream — as replayed from the journal — that ordering
    /// is fixed and byte-identical across processes, so the emitted
    /// `SequencerResult::MassCancelled` payload is replay-stable without any
    /// price-level traversal. This differs from the price-then-sequence order
    /// used by [`Self::cancel_all_orders`] and [`Self::cancel_orders_by_side`],
    /// but is equally deterministic.
    ///
    /// After a snapshot restore (`restore_from_snapshot_package`) the
    /// `user_orders` index is rebuilt from the resting-order layout rather than
    /// replayed from admission events, so a by-user cancel issued post-restore
    /// does **not** follow the original admission history. It is still fully
    /// deterministic: the rebuild walks price levels in the same fixed
    /// price-then-insertion-sequence order as [`Self::cancel_all_orders`] (bids
    /// ascending price, then asks ascending price; within each level ascending
    /// insertion sequence via `PriceLevel::snapshot_by_seq_into`), so the
    /// per-user admission order — and therefore this method's
    /// [`MassCancelResult::cancelled_order_ids`] — is byte-identical across every
    /// restore of the same package (#192). The order simply reflects the resting
    /// book at snapshot time, not the sequence in which the user's orders were
    /// originally admitted.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Hash32, Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// let user_a = Hash32::new([1u8; 32]);
    /// let user_b = Hash32::new([2u8; 32]);
    ///
    /// book.add_limit_order_with_user(
    ///     Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, user_a, None,
    /// )?;
    /// book.add_limit_order_with_user(
    ///     Id::from_uuid(Uuid::new_v4()), 110, 5, Side::Sell, TimeInForce::Gtc, user_b, None,
    /// )?;
    ///
    /// let result = book.cancel_orders_by_user(user_a);
    /// assert_eq!(result.cancelled_count(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn cancel_orders_by_user(&self, user_id: Hash32) -> MassCancelResult {
        // #248: exclusive submit gate. The scope is collected first and
        // cancelled id by id afterwards; under the shared side an id could be
        // cancelled and re-admitted out of scope in between (other side,
        // other price, other user, not expired) and would then be cancelled
        // by this call. Exclusive also keeps a concurrent FOK / STP window
        // (#209 / #225) out of the walk, as the shared side did.
        let _gate = self.submit_gate_write();
        trace!(
            "Order book {}: Mass cancel orders for user {}",
            self.symbol, user_id
        );

        // O(1) lookup via the user_orders index — no full book scan needed.
        // #248: copy the ids, do not remove the entry. Each successful cancel
        // untracks its own id through the single-order cancel path, so an
        // order whose cancel fails stays in the index (and resting) instead
        // of becoming unreachable by a later by-user cancel. The shard guard
        // is released at the end of this statement, before any cancel takes
        // the same shard's write side.
        let mut order_ids = self
            .user_orders
            .get(&user_id)
            .map(|entry| entry.value().to_vec())
            .unwrap_or_default();
        // #286: then the user's pending stops (they are not in the user
        // index), in admission order.
        order_ids.extend(self.stops_in_scope(|stop| stop.user_id() == user_id));
        if order_ids.is_empty() {
            return MassCancelResult::default();
        }

        let result =
            self.cancel_order_batch_with_reason(&order_ids, CancelReason::MassCancelByUser);
        self.purge_stale_user_ids(user_id);
        result
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
    /// An order whose price level refuses the removal is recorded as
    /// [`MassCancelFailure::OrderCancelFailed`] and stays resting and
    /// tracked; the call carries on with the rest of its scope (#248).
    ///
    /// # Concurrency
    ///
    /// Holds the **exclusive** side of the submit gate (#248): the scope is
    /// collected before it is cancelled, and an id re-admitted out of scope
    /// in between must not be cancelled by this call.
    ///
    /// # Determinism
    ///
    /// [`MassCancelResult::cancelled_order_ids`] — and therefore the journaled
    /// `SequencerResult::MassCancelled` payload — follows one fixed,
    /// replay-stable order over the levels in `[min_price, max_price]`, matching
    /// the [`Self::evict_expired_orders`] contract restricted to that range:
    ///
    /// 1. Price levels in **ascending price** order (the `SkipMap`'s ordered
    ///    range iteration — no sorting required).
    /// 2. Within a price level, orders in **ascending insertion sequence** —
    ///    the exact order the matching engine consumes resting orders
    ///    (`PriceLevel::snapshot_by_seq_into`), i.e. oldest first.
    ///
    /// The traversal never observes the `order_locations` / `user_orders`
    /// `DashMap` iteration order (seeded per instance), so the id sequence is
    /// byte-identical across processes and replay.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let book: OrderBook<()> = OrderBook::new("TEST");
    /// book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None)?;
    /// book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 200, 10, Side::Buy, TimeInForce::Gtc, None)?;
    /// book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 300, 10, Side::Buy, TimeInForce::Gtc, None)?;
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
        // #248: exclusive submit gate. The scope is collected first and
        // cancelled id by id afterwards; under the shared side an id could be
        // cancelled and re-admitted out of scope in between (other side,
        // other price, other user, not expired) and would then be cancelled
        // by this call. Exclusive also keeps a concurrent FOK / STP window
        // (#209 / #225) out of the walk, as the shared side did.
        let _gate = self.submit_gate_write();
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

        // Collect in the determinism-contract order: ascending price (the
        // `SkipMap`'s ordered range iteration), and within each level ascending
        // insertion sequence via `snapshot_by_seq_into` — never the
        // non-deterministic `iter_orders` view, which would make the journaled
        // `MassCancelled` payload diverge across processes. One scratch buffer is
        // reused across levels.
        //
        // Every level in range is read before any cancel runs; an unreadable
        // level refuses the whole call (see `MassCancelFailure`).
        let mut order_ids = Vec::new();
        let mut level_orders: Vec<Arc<OrderType<()>>> = Vec::new();
        for entry in price_levels.range(min_price..=max_price) {
            if let Err(failure) =
                read_level_orders(entry.value(), side, *entry.key(), &mut level_orders)
            {
                return self.refuse_mass_cancel(failure);
            }
            for order in &level_orders {
                order_ids.push(order.id());
            }
        }
        // #286: then the side's pending stops whose stop price is in range,
        // in admission order.
        order_ids.extend(self.stops_in_scope(|stop| {
            let stop_price = stop.price().as_u128();
            stop.side() == side && stop_price >= min_price && stop_price <= max_price
        }));

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
    /// The returned [`EvictionResult`] — and the [`PriceLevelChangedEvent`](super::book_change_event::PriceLevelChangedEvent) and
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
    /// A second sweep at the same `now_ms` returns an empty result: the
    /// expired orders are already gone.
    ///
    /// # Returns
    ///
    /// An [`EvictionResult`] with the evicted orders and their ids, in the
    /// deterministic order above (empty when nothing was expired), plus any
    /// per-order failure (#248). The sweep does not stop at a failure: every
    /// other expired order is still evicted, with its usual events.
    /// [`MassCancelFailure::OrderCancelFailed`] marks an order that is still
    /// resting and fully tracked (a later sweep retries it);
    /// [`MassCancelFailure::LevelFaultAfterRemoval`] marks one that was
    /// evicted but whose level then failed. Journal the outcome with
    /// [`EvictionResult::mass_cancel_result`] as
    /// `SequencerResult::MassCancelled`: replay then reproduces exactly the
    /// journaled evictions.
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::PriceLevelError`] when a price level's
    /// resting orders cannot be read (`PriceLevel::snapshot_by_seq_into` is
    /// fallible since pricelevel 0.10). The read phase runs before any
    /// eviction, so on this `Err` nothing was evicted and the book is
    /// unchanged.
    ///
    /// # Concurrency
    ///
    /// Holds the **exclusive** side of the submit gate (#248), like every
    /// mass cancel: the expired ids are collected before they are removed,
    /// and an id re-admitted in between as a live order must not be evicted.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::{Clock, OrderBook, StubClock};
    /// use pricelevel::{Id, Side, TimeInForce, TimestampMs};
    /// use std::sync::Arc;
    /// use uuid::Uuid;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// // A logical clock starting at 0 so the small GTD deadline is admitted
    /// // (wall-clock admission would treat it as already expired).
    /// let book: OrderBook<()> =
    ///     OrderBook::with_clock("TEST", Arc::new(StubClock::starting_at(0)) as Arc<dyn Clock>);
    /// let gtd = Id::from_uuid(Uuid::new_v4());
    /// // A resting GTD order that expires at t = 1_000 ms.
    /// book.add_limit_order(gtd, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)?;
    ///
    /// // Nothing expired yet at t = 999.
    /// assert!(book.evict_expired_orders(TimestampMs::new(999))?.is_empty());
    ///
    /// // At the deadline the order is evicted and no longer rests.
    /// let evicted = book.evict_expired_orders(TimestampMs::new(1_000))?;
    /// assert_eq!(evicted.len(), 1);
    /// assert_eq!(book.best_bid(), None);
    ///
    /// // Idempotent: a second sweep at the same instant evicts nothing.
    /// assert!(book.evict_expired_orders(TimestampMs::new(1_000))?.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub fn evict_expired_orders(
        &self,
        now_ms: TimestampMs,
    ) -> Result<EvictionResult<T>, OrderBookError> {
        // #248: exclusive submit gate. The scope is collected first and
        // cancelled id by id afterwards; under the shared side an id could be
        // cancelled and re-admitted out of scope in between (other side,
        // other price, other user, not expired) and would then be cancelled
        // by this call. Exclusive also keeps a concurrent FOK / STP window
        // (#209 / #225) out of the walk, as the shared side did.
        let _gate = self.submit_gate_write();
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
        //
        // Phase 1 mutates nothing, so a level that cannot be read returns the
        // error before any order is evicted (all-or-nothing, like the mass
        // cancels' `MassCancelFailure::LevelUnreadable`).
        let mut expired_ids: Vec<Id> = Vec::new();
        let mut level_orders: Vec<Arc<OrderType<()>>> = Vec::new();
        let sides = self
            .bids
            .iter()
            .map(|entry| (entry, Side::Buy))
            .chain(self.asks.iter().map(|entry| (entry, Side::Sell)));
        for (entry, side) in sides {
            if let Err(failure) =
                read_level_orders(entry.value(), side, *entry.key(), &mut level_orders)
            {
                tracing::warn!(
                    symbol = %self.symbol,
                    now_ms = now,
                    %failure,
                    "expired-order eviction refused: price level unreadable"
                );
                return Err(failure.to_order_book_error());
            }
            for order in &level_orders {
                if self.tif_expired_at(order.time_in_force(), now) {
                    expired_ids.push(order.id());
                }
            }
        }
        // #286: then the expired pending stops, in admission order.
        expired_ids
            .extend(self.stops_in_scope(|stop| self.tif_expired_at(stop.time_in_force(), now)));

        if expired_ids.is_empty() {
            return Ok(EvictionResult::default());
        }

        // Phase 2: cancel each expired order through the shared single-order
        // path, preserving the collection order. This is what keeps the caches,
        // trackers, and emitted events consistent and in the documented order.
        //
        // #248: a per-order failure is not swallowed: it is recorded in the
        // result and the sweep carries on with the next expired order (each
        // removal is independent).
        let mut evicted = Vec::with_capacity(expired_ids.len());
        let result = self.cancel_batch(
            &expired_ids,
            CancelReason::TimeInForceExpired,
            Some(&mut evicted),
        );

        trace!(
            symbol = %self.symbol,
            now_ms = now,
            evicted = result.cancelled_count(),
            failed = result.failures().len(),
            "expired orders evicted"
        );

        Ok(EvictionResult { evicted, result })
    }

    /// Replays a journaled eviction: removes exactly `order_ids` as
    /// [`CancelReason::TimeInForceExpired`], in the given order, under the
    /// exclusive submit gate (#248).
    ///
    /// Replay applies the journaled identities instead of re-running the
    /// sweep, so an order the live sweep failed to evict is not evicted on
    /// replay either. The caller compares the returned ids with `order_ids`.
    pub(crate) fn evict_orders_by_id(&self, order_ids: &[Id]) -> MassCancelResult {
        let _gate = self.submit_gate_write();
        self.cancel_batch(order_ids, CancelReason::TimeInForceExpired, None)
    }

    /// Internal helper: cancel a batch of orders by their IDs with a reason.
    ///
    /// See [`Self::cancel_batch`].
    fn cancel_order_batch_with_reason(
        &self,
        order_ids: &[Id],
        reason: CancelReason,
    ) -> MassCancelResult {
        self.cancel_batch(order_ids, reason, None)
    }

    /// Cancels `order_ids` one by one through
    /// [`Self::cancel_order_with_reason`], in the given order, and pushes the
    /// removed bodies into `bodies` when given.
    ///
    /// Outcomes per id:
    /// - cancelled: listed in the result's ids;
    /// - the level refused the removal: recorded as
    ///   [`MassCancelFailure::OrderCancelFailed`]; the order stays resting
    ///   and tracked;
    /// - the level removed the order, then failed
    ///   ([`OrderBookError::OrderRemovedWithLevelFault`]): listed in the ids
    ///   (it is gone and the book completed the removal) **and** recorded as
    ///   [`MassCancelFailure::LevelFaultAfterRemoval`]; no body exists for it;
    /// - no longer resting: skipped and logged (callers hold the exclusive
    ///   submit gate, so only an index inconsistency gets there).
    ///
    /// The batch always carries on with the next id.
    fn cancel_batch(
        &self,
        order_ids: &[Id],
        reason: CancelReason,
        mut bodies: Option<&mut Vec<Arc<OrderType<T>>>>,
    ) -> MassCancelResult {
        let mut cancelled_ids = Vec::with_capacity(order_ids.len());
        let mut failures = Vec::new();

        for &order_id in order_ids {
            // cancel_order_with_reason handles: listener notification, special
            // order cleanup, empty level removal, order_locations / user_orders
            // cleanup, risk release and state tracking.
            match self.cancel_order_with_reason(order_id, reason) {
                Ok(Some(order)) => {
                    cancelled_ids.push(order_id);
                    if let Some(bodies) = bodies.as_deref_mut() {
                        bodies.push(order);
                    }
                }
                Ok(None) => self.note_order_not_in_book(order_id),
                Err(OrderBookError::OrderRemovedWithLevelFault { order_id, source }) => {
                    cancelled_ids.push(order_id);
                    failures.push(MassCancelFailure::LevelFaultAfterRemoval {
                        order_id,
                        error: *source,
                    });
                }
                Err(error) => {
                    let failure = MassCancelFailure::order_cancel_failed(order_id, error);
                    tracing::warn!(
                        symbol = %self.symbol,
                        %reason,
                        %failure,
                        "mass cancel could not cancel an order; it stays resting"
                    );
                    failures.push(failure);
                }
            }
        }

        MassCancelResult::with_failures(cancelled_ids, failures)
    }

    /// Drops the ids of `user_id`'s index entry that no longer name a
    /// resting order, after a by-user cancel (#248).
    ///
    /// A successful cancel already untracked its own id, and a failed one is
    /// still resting, so only stale ids — an index inconsistency, logged by
    /// [`Self::note_order_not_in_book`] — are dropped here. The pre-#248
    /// code removed the whole entry up front, which also purged them; this
    /// keeps that repair without dropping failed orders. Runs under the
    /// exclusive submit gate, so no admission can race the check.
    fn purge_stale_user_ids(&self, user_id: Hash32) {
        let now_empty = match self.user_orders.get_mut(&user_id) {
            None => return,
            Some(mut entry) => {
                // Different map from `user_orders`, so reading it under this
                // shard guard cannot deadlock.
                entry
                    .value_mut()
                    .retain(|id| self.order_locations.contains_key(id));
                entry.value().is_empty()
            }
        };
        if now_empty {
            self.user_orders.remove(&user_id);
        }
    }

    /// Logs a scoped mass cancel or eviction that found a collected id no
    /// longer resting. Under the exclusive submit gate this means the
    /// `order_locations` / `user_orders` indices disagree with the levels.
    #[cold]
    #[inline(never)]
    fn note_order_not_in_book(&self, order_id: Id) {
        tracing::warn!(
            symbol = %self.symbol,
            %order_id,
            "mass cancel: order in scope is not resting at its indexed location; index inconsistency, skipped"
        );
    }

    /// Collect all order IDs on a given side by iterating price levels in the
    /// deterministic sweep order.
    ///
    /// Levels are visited in ascending price (the `SkipMap`'s natural key
    /// order) and orders within a level in ascending insertion sequence via
    /// `PriceLevel::snapshot_by_seq_into` — the exact order the matching engine
    /// consumes resting orders. The non-deterministic `iter_orders` view is
    /// deliberately avoided so the resulting id sequence (which lands in the
    /// journaled `MassCancelled` payload) is replay-stable across processes. One
    /// scratch buffer is reused across levels to avoid a per-level allocation.
    ///
    /// Stops at the first level whose orders cannot be read and returns it as
    /// a [`MassCancelFailure`]; nothing has been cancelled at that point.
    fn collect_order_ids_by_side(&self, side: Side) -> Result<Vec<Id>, MassCancelFailure> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        let mut ids = Vec::new();
        let mut level_orders: Vec<Arc<OrderType<()>>> = Vec::new();
        for entry in price_levels.iter() {
            read_level_orders(entry.value(), side, *entry.key(), &mut level_orders)?;
            for order in &level_orders {
                ids.push(order.id());
            }
        }
        Ok(ids)
    }

    /// The pending trailing stops matching `keep`, in admission order: the
    /// part of a mass cancel's or an expiry eviction's scope that rests on
    /// no level (#286), appended after the resting orders it collected.
    /// Empty without `special_orders` or when no stop is pending.
    fn stops_in_scope(&self, keep: impl Fn(&OrderType<()>) -> bool) -> Vec<Id> {
        #[cfg(feature = "special_orders")]
        if self.has_pending_stops() {
            return self.pending_stop_ids(keep);
        }
        #[cfg(not(feature = "special_orders"))]
        let _ = keep;
        Vec::new()
    }

    /// Logs a refused mass cancel and builds its result: nothing cancelled,
    /// `failure` recorded.
    #[cold]
    #[inline(never)]
    fn refuse_mass_cancel(&self, failure: MassCancelFailure) -> MassCancelResult {
        tracing::warn!(
            symbol = %self.symbol,
            %failure,
            "mass cancel refused: price level unreadable, nothing cancelled"
        );
        MassCancelResult::refused(failure)
    }
}

/// The price-level error behind a failed single-order cancel.
/// `cancel_order_with_reason` only fails with
/// [`OrderBookError::PriceLevelError`]; any other variant is folded into
/// [`PriceLevelError::InvalidOperation`] carrying its message, so the failure
/// is still recorded rather than dropped.
#[cold]
#[inline(never)]
fn cancel_error_source(error: OrderBookError) -> PriceLevelError {
    match error {
        OrderBookError::PriceLevelError(error) => error,
        other => PriceLevelError::InvalidOperation {
            message: other.to_string(),
        },
    }
}

/// Reads `level`'s resting orders into `buf` in ascending insertion sequence
/// (the sweep order), mapping a pricelevel failure to the
/// [`MassCancelFailure`] a mass cancel records. On `Err` pricelevel leaves
/// `buf` untouched, so it still holds the previous level's orders and must
/// not be consumed.
#[inline]
fn read_level_orders(
    level: &PriceLevel,
    side: Side,
    price: u128,
    buf: &mut Vec<Arc<OrderType<()>>>,
) -> Result<(), MassCancelFailure> {
    level
        .snapshot_by_seq_into(buf)
        .map_err(|error| MassCancelFailure::LevelUnreadable { side, price, error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::TimeInForce;

    /// Fresh random order id (UUID v4).
    fn new_id() -> Id {
        Id::from_uuid(uuid::Uuid::new_v4())
    }

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

    fn unreadable_level() -> MassCancelFailure {
        MassCancelFailure::LevelUnreadable {
            side: Side::Sell,
            price: 101,
            error: PriceLevelError::CapacityExceeded {
                resource: pricelevel::CapacityResource::OrderSnapshot,
                additional: 3,
            },
        }
    }

    #[test]
    fn test_mass_cancel_result_refused_reports_failure_and_cancels_nothing() {
        let result = MassCancelResult::refused(unreadable_level());
        assert!(result.is_empty());
        assert_eq!(result.cancelled_count(), 0);
        assert!(result.cancelled_order_ids().is_empty());
        assert!(result.has_failures());
        assert_eq!(result.failures(), &[unreadable_level()]);
        assert_eq!(
            result.to_string(),
            "MassCancelResult { cancelled: 0, failures: 1 }"
        );
        assert!(matches!(
            result.failures()[0].to_order_book_error(),
            OrderBookError::PriceLevelError(PriceLevelError::CapacityExceeded {
                additional: 3,
                ..
            })
        ));
        assert!(
            result.failures()[0]
                .to_string()
                .starts_with("price level SELL 101 unreadable")
        );
    }

    #[test]
    fn test_mass_cancel_result_failures_round_trip_json() {
        let result = MassCancelResult::refused(unreadable_level());
        let json = serde_json::to_string(&result).expect("serialize");
        assert!(json.contains("\"level_unreadable\""), "{json}");
        let decoded: MassCancelResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.failures(), result.failures());
    }

    /// JSON in the pre-#248 0.14.0 shape — a `failures` list holding only
    /// `level_unreadable` entries — still decodes unchanged: #248 only adds
    /// a variant (#248 serde back-compat).
    #[test]
    fn test_mass_cancel_result_json_without_order_cancel_failed_decodes() {
        let pre_248 = r#"{"cancelled_count":0,"cancelled_order_ids":[],"failures":[{"level_unreadable":{"side":"SELL","price":101,"error":{"CapacityExceeded":{"resource":"order_snapshot","additional":3}}}}]}"#;
        let decoded: MassCancelResult = serde_json::from_str(pre_248).expect("pre-#248 json");
        assert_eq!(decoded.failures(), &[unreadable_level()]);
        assert!(decoded.is_refused());
        assert!(decoded.is_empty());
    }

    fn order_cancel_failed(order: u64) -> MassCancelFailure {
        MassCancelFailure::OrderCancelFailed {
            order_id: Id::from_u64(order),
            error: PriceLevelError::InvalidOperation {
                message: "refused".to_string(),
            },
        }
    }

    /// A partial result (cancelled ids plus per-order failures) round-trips
    /// through JSON, is not a refusal, and renders its failures.
    #[test]
    fn test_mass_cancel_result_partial_round_trip_json() {
        let result = MassCancelResult::with_failures(
            vec![Id::from_u64(1), Id::from_u64(3)],
            vec![order_cancel_failed(2)],
        );
        assert_eq!(result.cancelled_count(), 2);
        assert!(result.has_failures());
        assert!(!result.is_refused());
        assert_eq!(
            result.to_string(),
            "MassCancelResult { cancelled: 2, failures: 1 }"
        );
        let failure = &result.failures()[0];
        assert!(!failure.is_refusal());
        assert!(
            failure
                .to_string()
                .starts_with(&format!("order {} not cancelled", Id::from_u64(2)))
        );
        assert!(matches!(
            failure.to_order_book_error(),
            OrderBookError::PriceLevelError(PriceLevelError::InvalidOperation { .. })
        ));

        let json = serde_json::to_string(&result).expect("serialize");
        assert!(json.contains("\"order_cancel_failed\""), "{json}");
        let decoded: MassCancelResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.cancelled_order_ids(), result.cancelled_order_ids());
        assert_eq!(decoded.cancelled_count(), 2);
        assert_eq!(decoded.failures(), result.failures());
    }

    /// A non-price-level cancel error is folded into
    /// `PriceLevelError::InvalidOperation` rather than dropped.
    #[test]
    fn test_order_cancel_failed_folds_other_errors() {
        let failure = MassCancelFailure::order_cancel_failed(
            Id::from_u64(9),
            OrderBookError::InvalidOperation {
                message: "boom".to_string(),
            },
        );
        match failure {
            MassCancelFailure::OrderCancelFailed {
                order_id,
                error: PriceLevelError::InvalidOperation { message },
            } => {
                assert_eq!(order_id, Id::from_u64(9));
                assert!(message.contains("boom"), "{message}");
            }
            other => panic!("unexpected failure {other:?}"),
        }
    }

    /// JSON written before 0.14 (no `failures` key) decodes with an empty
    /// failure list.
    #[test]
    fn test_mass_cancel_result_legacy_json_without_failures_decodes() {
        let legacy = r#"{"cancelled_count":1,"cancelled_order_ids":["00000000-0000-0007-0000-000000000000"]}"#;
        let decoded: MassCancelResult = serde_json::from_str(legacy).expect("legacy json");
        assert_eq!(decoded.cancelled_count(), 1);
        assert_eq!(decoded.cancelled_order_ids(), &[Id::from_u64(7)]);
        assert!(!decoded.has_failures());
    }

    #[test]
    fn test_mass_cancels_complete_without_failures() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        book.add_limit_order(new_id(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");
        book.add_limit_order(new_id(), 110, 5, Side::Sell, TimeInForce::Gtc, None)
            .expect("add ask");
        assert!(
            !book
                .cancel_orders_by_price_range(Side::Buy, 0, 200)
                .has_failures()
        );
        assert!(!book.cancel_orders_by_side(Side::Sell).has_failures());
        book.add_limit_order(new_id(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");
        let all = book.cancel_all_orders();
        assert!(!all.has_failures());
        assert_eq!(all.cancelled_count(), 1);
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

        let id1 = new_id();
        let id2 = new_id();
        let id3 = new_id();

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

        book.add_limit_order(new_id(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");
        book.add_limit_order(new_id(), 95, 5, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid 2");
        book.add_limit_order(new_id(), 200, 8, Side::Sell, TimeInForce::Gtc, None)
            .expect("add ask");

        let result = book.cancel_orders_by_side(Side::Buy);
        assert_eq!(result.cancelled_count(), 2);
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), Some(200));
    }

    #[test]
    fn test_cancel_by_side_sell() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        book.add_limit_order(new_id(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");
        book.add_limit_order(new_id(), 200, 8, Side::Sell, TimeInForce::Gtc, None)
            .expect("add ask");
        book.add_limit_order(new_id(), 210, 3, Side::Sell, TimeInForce::Gtc, None)
            .expect("add ask 2");

        let result = book.cancel_orders_by_side(Side::Sell);
        assert_eq!(result.cancelled_count(), 2);
        assert_eq!(book.best_bid(), Some(100));
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn test_cancel_by_side_empty() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        book.add_limit_order(new_id(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
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

        let id_a1 = new_id();
        let id_a2 = new_id();
        let id_b1 = new_id();

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
            new_id(),
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

        let id1 = new_id();
        let id2 = new_id();
        let id3 = new_id();

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

        book.add_limit_order(new_id(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add");

        // min > max → no cancellation
        let result = book.cancel_orders_by_price_range(Side::Buy, 200, 100);
        assert!(result.is_empty());
        assert_eq!(book.order_locations.len(), 1);
    }

    #[test]
    fn test_cancel_by_price_range_no_match() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        book.add_limit_order(new_id(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add");

        let result = book.cancel_orders_by_price_range(Side::Buy, 200, 300);
        assert!(result.is_empty());
        assert_eq!(book.order_locations.len(), 1);
    }

    #[test]
    fn test_cancel_by_price_range_exact_boundaries() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let id1 = new_id();
        let id2 = new_id();

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

        let id1 = new_id();
        let id2 = new_id();

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

        let id1 = new_id();
        let id2 = new_id();

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
        book.add_limit_order_with_user(new_id(), 100, 10, Side::Buy, TimeInForce::Gtc, user, None)
            .expect("add buy 100");
        book.add_limit_order_with_user(new_id(), 95, 5, Side::Buy, TimeInForce::Gtc, user, None)
            .expect("add buy 95");
        book.add_limit_order_with_user(new_id(), 200, 8, Side::Sell, TimeInForce::Gtc, user, None)
            .expect("add sell 200");
        book.add_limit_order_with_user(new_id(), 90, 20, Side::Buy, TimeInForce::Gtc, other, None)
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

        let id1 = new_id();
        let id2 = new_id();
        let id3 = new_id();

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

        let id1 = new_id();
        let id2 = new_id();

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

    // ---- deterministic result ordering (#190) ----------------------------

    /// A book with two price levels per side and two orders per level, admitted
    /// in an interleaved order so the `order_locations` / `user_orders` DashMap
    /// insertion order is scrambled relative to the price-then-insertion-seq
    /// contract order. Returns the ids labelled `<side><price><a|b>` where `a`
    /// is the earlier admission at that level.
    struct InterleavedFixture {
        book: OrderBook<()>,
        b90a: Id,
        b90b: Id,
        b100a: Id,
        b100b: Id,
        a110a: Id,
        a110b: Id,
        a120a: Id,
        a120b: Id,
    }

    fn interleaved_fixture() -> InterleavedFixture {
        let book: OrderBook<()> = OrderBook::new("TEST");
        let b90a = new_id();
        let b90b = new_id();
        let b100a = new_id();
        let b100b = new_id();
        let a110a = new_id();
        let a110b = new_id();
        let a120a = new_id();
        let a120b = new_id();

        // Interleaved admission across sides and levels. The `a` order at each
        // level is admitted before its `b` sibling, so ascending insertion
        // sequence within a level is `a` then `b`.
        book.add_limit_order(b100a, 100, 1, Side::Buy, TimeInForce::Gtc, None)
            .expect("b100a");
        book.add_limit_order(a120a, 120, 1, Side::Sell, TimeInForce::Gtc, None)
            .expect("a120a");
        book.add_limit_order(b90a, 90, 1, Side::Buy, TimeInForce::Gtc, None)
            .expect("b90a");
        book.add_limit_order(a110a, 110, 1, Side::Sell, TimeInForce::Gtc, None)
            .expect("a110a");
        book.add_limit_order(b100b, 100, 1, Side::Buy, TimeInForce::Gtc, None)
            .expect("b100b");
        book.add_limit_order(a110b, 110, 1, Side::Sell, TimeInForce::Gtc, None)
            .expect("a110b");
        book.add_limit_order(b90b, 90, 1, Side::Buy, TimeInForce::Gtc, None)
            .expect("b90b");
        book.add_limit_order(a120b, 120, 1, Side::Sell, TimeInForce::Gtc, None)
            .expect("a120b");

        InterleavedFixture {
            book,
            b90a,
            b90b,
            b100a,
            b100b,
            a110a,
            a110b,
            a120a,
            a120b,
        }
    }

    #[test]
    fn test_cancel_orders_by_side_interleaved_returns_price_then_seq_order() {
        let f = interleaved_fixture();

        let buy = f.book.cancel_orders_by_side(Side::Buy);
        // Ascending price (90 then 100), FIFO within each level.
        assert_eq!(
            buy.cancelled_order_ids(),
            &[f.b90a, f.b90b, f.b100a, f.b100b]
        );

        let sell = f.book.cancel_orders_by_side(Side::Sell);
        assert_eq!(
            sell.cancelled_order_ids(),
            &[f.a110a, f.a110b, f.a120a, f.a120b]
        );
    }

    #[test]
    fn test_cancel_orders_by_price_range_interleaved_returns_price_then_seq_order() {
        let f = interleaved_fixture();

        // Single level: FIFO within the 90 level only.
        let single = f.book.cancel_orders_by_price_range(Side::Buy, 90, 90);
        assert_eq!(single.cancelled_order_ids(), &[f.b90a, f.b90b]);

        // Remaining bid level (100) via a wider range that spans it.
        let rest = f.book.cancel_orders_by_price_range(Side::Buy, 90, 200);
        assert_eq!(rest.cancelled_order_ids(), &[f.b100a, f.b100b]);
    }

    #[test]
    fn test_cancel_all_orders_interleaved_returns_bids_then_asks_order() {
        let f = interleaved_fixture();

        let all = f.book.cancel_all_orders();
        // Bids ascending (90, then 100), then asks ascending (110, then 120);
        // FIFO within every level.
        assert_eq!(
            all.cancelled_order_ids(),
            &[
                f.b90a, f.b90b, f.b100a, f.b100b, f.a110a, f.a110b, f.a120a, f.a120b,
            ]
        );
        assert_eq!(all.cancelled_count(), 8);
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
                .expect("evict")
                .is_empty()
        );
    }

    #[test]
    fn test_evict_expired_gtd_removed_and_unmatchable() {
        let book = expiring_book();
        let gtd = new_id();
        book.add_limit_order(gtd, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("add gtd");

        // Before the deadline: untouched.
        assert!(
            book.evict_expired_orders(TimestampMs::new(999))
                .expect("evict")
                .is_empty()
        );
        assert_eq!(book.best_bid(), Some(100));

        // At the deadline (>=): evicted.
        let evicted = book
            .evict_expired_orders(TimestampMs::new(1_000))
            .expect("evict");
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted.evicted_orders()[0].id(), gtd);
        assert_eq!(book.best_bid(), None);
        assert!(!book.order_locations.contains_key(&gtd));

        // No longer matchable: a crossing sell finds no liquidity.
        let taker = new_id();
        assert!(book.match_market_order(taker, 10, Side::Sell).is_err());
    }

    #[test]
    fn test_evict_expired_leaves_gtc_and_unexpired_gtd_untouched() {
        let book = expiring_book();
        let gtc = new_id();
        let gtd_future = new_id();
        let gtd_past = new_id();
        book.add_limit_order(gtc, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("gtc");
        book.add_limit_order(gtd_future, 99, 5, Side::Buy, TimeInForce::Gtd(5_000), None)
            .expect("gtd future");
        book.add_limit_order(gtd_past, 98, 5, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("gtd past");

        let evicted = book
            .evict_expired_orders(TimestampMs::new(2_000))
            .expect("evict");
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted.evicted_orders()[0].id(), gtd_past);

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
        let id = new_id();
        book.add_limit_order(id, 100, 10, Side::Sell, TimeInForce::Gtd(1_000), None)
            .expect("add");

        // 999 -> not expired.
        assert!(
            book.evict_expired_orders(TimestampMs::new(999))
                .expect("evict")
                .is_empty()
        );
        // Exactly at the deadline -> evicted.
        let evicted = book
            .evict_expired_orders(TimestampMs::new(1_000))
            .expect("evict");
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted.evicted_orders()[0].id(), id);
    }

    #[test]
    fn test_evict_expired_day_uses_market_close() {
        let book = expiring_book();
        book.set_market_close_timestamp(2_000);
        let day = new_id();
        book.add_limit_order(day, 100, 10, Side::Buy, TimeInForce::Day, None)
            .expect("day");

        // Before close: untouched.
        assert!(
            book.evict_expired_orders(TimestampMs::new(1_999))
                .expect("evict")
                .is_empty()
        );
        // At/after close: evicted.
        let evicted = book
            .evict_expired_orders(TimestampMs::new(2_000))
            .expect("evict");
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted.evicted_orders()[0].id(), day);
    }

    #[test]
    fn test_evict_expired_deterministic_order_multiple_levels_sides() {
        let book = expiring_book();

        // Bids at two levels (ascending: 90 then 95), FIFO within each level.
        let b95a = new_id();
        let b95b = new_id();
        let b90 = new_id();
        book.add_limit_order(b95a, 95, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("b95a");
        book.add_limit_order(b95b, 95, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("b95b");
        book.add_limit_order(b90, 90, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("b90");

        // Asks at two levels (ascending: 100 then 110).
        let a100 = new_id();
        let a110 = new_id();
        book.add_limit_order(a100, 100, 1, Side::Sell, TimeInForce::Gtd(1_000), None)
            .expect("a100");
        book.add_limit_order(a110, 110, 1, Side::Sell, TimeInForce::Gtd(1_000), None)
            .expect("a110");

        let evicted = book
            .evict_expired_orders(TimestampMs::new(2_000))
            .expect("evict");
        let ids: Vec<Id> = evicted.iter().map(|o| o.id()).collect();

        // Contract: bids ascending (90, then 95 FIFO), then asks ascending.
        assert_eq!(ids, vec![b90, b95a, b95b, a100, a110]);
    }

    #[test]
    fn test_evict_expired_second_sweep_is_idempotent() {
        let book = expiring_book();
        let id = new_id();
        book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("add");

        let first = book
            .evict_expired_orders(TimestampMs::new(1_000))
            .expect("evict");
        assert_eq!(first.len(), 1);
        let second = book
            .evict_expired_orders(TimestampMs::new(1_000))
            .expect("evict");
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

        let id = new_id();
        book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("add");

        let evicted = book
            .evict_expired_orders(TimestampMs::new(1_000))
            .expect("evict");
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

        let id = new_id();
        book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("add");

        let evicted = book
            .evict_expired_orders(TimestampMs::new(1_000))
            .expect("evict");
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
