use crate::{OrderBook, OrderBookError};
use pricelevel::{OrderType, Side, TimeInForce};
use std::sync::atomic::Ordering;

impl<T> OrderBook<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// The market-close timestamp (Unix milliseconds) used for `Day`-order
    /// expiry, or `None` when no market close has been configured via
    /// [`Self::set_market_close_timestamp`].
    ///
    /// Single source of the market-close input shared by [`Self::has_expired`]
    /// and [`Self::tif_expired_at`] so admission and eviction agree on `Day`
    /// boundary behaviour.
    #[inline]
    pub(super) fn market_close_for_expiry(&self) -> Option<u64> {
        if self.has_market_close.load(Ordering::Relaxed) {
            Some(self.market_close_timestamp.load(Ordering::Relaxed))
        } else {
            None
        }
    }

    /// Whether a [`TimeInForce`] is expired at an explicit `now_ms`.
    ///
    /// This is the single definition of expiry in the book: both admission
    /// (via [`Self::has_expired`]) and the eviction sweep
    /// ([`Self::evict_expired_orders`](crate::OrderBook::evict_expired_orders))
    /// route through it, so they cannot disagree on the boundary case
    /// (`deadline == now_ms`, which counts as expired for `Gtd`, and
    /// `now_ms == market_close` for `Day`).
    ///
    /// `now_ms` is **milliseconds since the Unix epoch** — the same unit as a
    /// `Gtd` deadline and the market-close timestamp. A value expressed in
    /// seconds would be treated as a moment in 1970.
    #[inline]
    #[must_use]
    pub(super) fn tif_expired_at(&self, time_in_force: TimeInForce, now_ms: u64) -> bool {
        time_in_force.is_expired(now_ms, self.market_close_for_expiry())
    }

    /// Check if an order has expired **as of the book's own clock**.
    ///
    /// The comparison unit is **milliseconds since the Unix epoch**: the
    /// current time comes from `self.clock().now_millis()`, and a `Gtd`
    /// order's deadline (and the market-close timestamp used for `Day` orders,
    /// see [`Self::set_market_close_timestamp`]) must be supplied in the same
    /// unit. A deadline expressed in seconds would be treated as a moment in
    /// 1970 and the order would read as instantly expired.
    ///
    /// The boundary predicate (`now >= deadline` for `Gtd`,
    /// `now >= market_close` for `Day`) is defined once internally and shared
    /// with the caller-supplied-timestamp eviction sweep
    /// ([`Self::evict_expired_orders`](crate::OrderBook::evict_expired_orders)),
    /// so admission and eviction never disagree.
    pub fn has_expired(&self, order: &OrderType<T>) -> bool {
        let current_time = self.clock().now_millis().as_u64();
        self.tif_expired_at(order.time_in_force(), current_time)
    }

    /// Check if there would be a price crossing
    pub fn will_cross_market(&self, price: u128, side: Side) -> bool {
        match side {
            Side::Buy => OrderBook::<T>::best_ask(self).is_some_and(|best_ask| price >= best_ask),
            Side::Sell => OrderBook::<T>::best_bid(self).is_some_and(|best_bid| price <= best_bid),
        }
    }

    /// Register an order in the `user_orders` index and return the sequence
    /// it is tracked under, for its [`OrderLocation`](super::book::OrderLocation)
    /// to carry.
    ///
    /// Orders with `Hash32::zero()` (anonymous) are still tracked so that
    /// `cancel_all_orders` and `cancel_orders_by_side` work correctly.
    #[inline]
    pub(super) fn track_user_order(
        &self,
        user_id: pricelevel::Hash32,
        order_id: pricelevel::Id,
    ) -> u64 {
        self.user_orders
            .entry(user_id)
            .or_default()
            .insert(order_id)
    }

    /// Remove an order, tracked under `user_seq`, from the `user_orders`
    /// index.
    ///
    /// If the user's order list becomes empty, the entry is removed entirely.
    #[inline]
    pub(super) fn untrack_user_order(
        &self,
        user_id: pricelevel::Hash32,
        user_seq: u64,
        order_id: &pricelevel::Id,
    ) {
        let emptied = match self.user_orders.get_mut(&user_id) {
            Some(mut entry) => {
                let ids = entry.value_mut();
                // A binary search on the sequence; the remaining ids keep
                // their admission order (#252).
                ids.remove(user_seq, order_id);
                ids.is_empty()
            }
            None => false,
        };
        if emptied {
            self.remove_user_if_empty(user_id);
        }
    }

    /// Remove a still-located order from the `user_orders` index, keyed by
    /// the owner and sequence its [`OrderLocation`](super::book::OrderLocation)
    /// carries.
    ///
    /// Used where the order already left its price level, so its body (and
    /// `user_id`) is no longer reachable from the level: the fill path for
    /// every fully filled maker, and a cancel whose level kept no body. One
    /// keyed `user_orders` lookup instead of the full scan of
    /// [`Self::untrack_order_by_id`] (#259: that scan ran once per filled
    /// maker, cost O(active users) and allocated one guard per shard it
    /// visited). Must run BEFORE the location is released (#288: the
    /// location is the id's ownership token). Falls back to the scan only
    /// if the location is already gone, which no caller reaches.
    #[inline]
    pub(super) fn untrack_located_order(&self, order_id: &pricelevel::Id) {
        let owner = self
            .order_locations
            .get(order_id)
            .map(|location| (location.user_id, location.user_seq));
        match owner {
            Some((user_id, user_seq)) => self.untrack_user_order(user_id, user_seq, order_id),
            None => self.untrack_order_by_id(order_id),
        }
    }

    /// Remove an order from the `user_orders` index by scanning all entries.
    ///
    /// Cold fallback of [`Self::untrack_located_order`] for an order whose
    /// location is already gone (unreachable in practice). O(active users):
    /// it visits every `user_orders` shard until it finds the id.
    ///
    /// The removal preserves the relative order of the user's remaining ids:
    /// `cancel_orders_by_user` walks them in admission order, and replay
    /// reconciles its result by id order (#252), so a fill must not reorder
    /// a user's resting orders.
    #[cold]
    #[inline(never)]
    pub(super) fn untrack_order_by_id(&self, order_id: &pricelevel::Id) {
        let mut user_to_remove = None;
        for mut entry in self.user_orders.iter_mut() {
            let ids = entry.value_mut();
            if ids.remove_by_id(order_id) {
                if ids.is_empty() {
                    user_to_remove = Some(*entry.key());
                }
                break;
            }
        }
        if let Some(user_id) = user_to_remove {
            self.remove_user_if_empty(user_id);
        }
    }

    /// Drops `user_id`'s `user_orders` entry if it is still empty (#288).
    ///
    /// The emptying guard is released before this runs, so a concurrent
    /// admission for the same user can push a new id in between; an
    /// unconditional `remove` would delete that live entry. `remove_if`
    /// re-checks emptiness under the shard's write lock.
    #[inline]
    fn remove_user_if_empty(&self, user_id: pricelevel::Hash32) {
        self.user_orders
            .remove_if(&user_id, |_, ids| ids.is_empty());
    }

    /// Record an order state transition if a tracker is configured,
    /// and emit operational metrics when the transition is a
    /// rejection.
    ///
    /// Tracker recording is a no-op when `order_state_tracker` is
    /// `None`. The tracker's listener, when installed, is not called here:
    /// the transition is buffered and delivered after commit (#249).
    /// Metrics emission is unconditional but compiles to a no-op when
    /// the `metrics` feature is disabled — see
    /// [`crate::orderbook::metrics`]. Hooking the metric here keeps
    /// every reject path in the engine on the same single emission
    /// point.
    #[inline]
    pub(super) fn track_state(
        &self,
        order_id: pricelevel::Id,
        status: super::order_state::OrderStatus,
    ) {
        if let super::order_state::OrderStatus::Rejected { reason } = &status {
            super::metrics::record_reject(*reason);
        }
        if let Some(ref tracker) = self.order_state_tracker
            && let Some((old, new)) = tracker.record_transition(order_id, status)
        {
            // #249: the tracker records now; its listener runs after the
            // mutation commits and the submit gate is released.
            self.defer_event(super::emission::PendingEvent::State { order_id, old, new });
        }
    }

    /// Undo a resting state [`Self::track_state`] recorded for an order
    /// whose admission then unwound (#294). No clock, no listener, no
    /// metric: safe to call from a drop guard while the thread unwinds.
    #[cold]
    #[inline(never)]
    pub(super) fn withdraw_tracked_state(
        &self,
        order_id: pricelevel::Id,
        status: &super::order_state::OrderStatus,
    ) {
        if let Some(ref tracker) = self.order_state_tracker {
            tracker.withdraw_last_transition(order_id, status);
        }
    }

    /// Record an `OrderStatus::Rejected` transition for a failed risk
    /// admission, mapping the typed [`OrderBookError`] to its closed
    /// [`super::reject_reason::RejectReason`] code.
    ///
    /// Used by new-flow entry points where the risk gate (`Risk*`
    /// variants) returned `Err` before the order could enter the book —
    /// emitting `Rejected` here keeps the lifecycle accurate (the
    /// `Rejected` semantic is "rejected during validation, never
    /// entered"). No-op when no tracker is configured.
    #[inline]
    pub(super) fn reject_with_risk(&self, order_id: pricelevel::Id, err: &OrderBookError) {
        // `track_state` is itself a no-op when no tracker is configured,
        // so the outer `is_some` check is redundant. Drop it to keep the
        // helper minimal and avoid future divergence.
        self.track_state(
            order_id,
            super::order_state::OrderStatus::Rejected {
                reason: super::reject_reason::RejectReason::from(err),
            },
        );
    }

    /// Convert `OrderType<T>` to OrderType<()> for compatibility with current PriceLevel API
    pub fn convert_to_unit_type(&self, order: &OrderType<T>) -> OrderType<()> {
        match order {
            OrderType::Standard {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::Standard {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: (),
            },
            OrderType::IcebergOrder {
                id,
                price,
                visible_quantity,
                hidden_quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::IcebergOrder {
                id: *id,
                price: *price,
                visible_quantity: *visible_quantity,
                hidden_quantity: *hidden_quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: (),
            },
            OrderType::PostOnly {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::PostOnly {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: (),
            },
            OrderType::TrailingStop {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                trail_amount,
                last_reference_price,
                ..
            } => OrderType::TrailingStop {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                trail_amount: *trail_amount,
                last_reference_price: *last_reference_price,
                extra_fields: (),
            },
            OrderType::PeggedOrder {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                reference_price_offset,
                reference_price_type,
                ..
            } => OrderType::PeggedOrder {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                reference_price_offset: *reference_price_offset,
                reference_price_type: *reference_price_type,
                extra_fields: (),
            },
            OrderType::MarketToLimit {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::MarketToLimit {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: (),
            },
            OrderType::ReserveOrder {
                id,
                price,
                visible_quantity,
                hidden_quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                replenish_threshold,
                replenish_amount,
                auto_replenish,
                ..
            } => OrderType::ReserveOrder {
                id: *id,
                price: *price,
                visible_quantity: *visible_quantity,
                hidden_quantity: *hidden_quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                replenish_threshold: *replenish_threshold,
                replenish_amount: *replenish_amount,
                auto_replenish: *auto_replenish,
                extra_fields: (),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::OrderBookError; // Import the error type
    use crate::orderbook::book::OrderBook;
    use crate::utils::current_time_millis; // Import the time utility
    use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};
    use uuid::Uuid;

    // Helper function to create a unique order ID
    fn create_order_id() -> Id {
        Id::from_uuid(Uuid::new_v4())
    }

    #[test]
    fn test_private_add_order_publishes_location_and_level() {
        let order_book: OrderBook<()> = OrderBook::new("TEST");
        let order_id = create_order_id();
        let order = OrderType::Standard {
            id: order_id,
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        assert!(order_book.add_order(order).is_ok());

        // Verify order location
        let location = order_book.order_locations.get(&order_id).unwrap();
        assert_eq!(location.value().price_side(), (100u128, Side::Buy));

        // Verify order in price level by checking its properties
        let price_level = order_book.bids.get(&100).unwrap();
        assert_eq!(price_level.value().order_count(), 1);
        assert_eq!(price_level.value().total_quantity().unwrap_or(0), 10); // Check if quantity matches the added order
    }

    #[test]
    fn test_will_cross_market_buy_no_ask() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // No ask orders yet, should not cross
        assert!(!book.will_cross_market(1000, Side::Buy));
    }

    // This test was missing its function definition
    #[test]
    fn test_has_expired_day_order() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        let current_time = current_time_millis();
        book.set_market_close_timestamp(current_time - 1000); // Set market close in the past

        let order = OrderType::Standard {
            id: create_order_id(),
            price: Price::new(1000),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time),
            time_in_force: TimeInForce::Day,
            extra_fields: (),
        };

        // Day order should expire if market close is in the past
        assert!(book.has_expired(&order));
    }

    #[test]
    fn test_will_cross_market_sell_no_bid() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // No bid orders yet, should not cross
        assert!(!book.will_cross_market(1000, Side::Sell));
    }

    #[test]
    fn test_will_cross_market_buy_with_cross() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a sell order at 1000
        let id = create_order_id();
        let result = book.add_limit_order(id, 1000, 10, Side::Sell, TimeInForce::Gtc, None);
        assert!(result.is_ok());

        // Buy at 1000 should cross
        assert!(book.will_cross_market(1000, Side::Buy));

        // Buy at 1001 should cross
        assert!(book.will_cross_market(1001, Side::Buy));

        // Buy at 999 should not cross
        assert!(!book.will_cross_market(999, Side::Buy));
    }

    #[test]
    fn test_will_cross_market_sell_with_cross() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a buy order at 1000
        let id = create_order_id();
        let result = book.add_limit_order(id, 1000, 10, Side::Buy, TimeInForce::Gtc, None);
        assert!(result.is_ok());

        // Sell at 1000 should cross
        assert!(book.will_cross_market(1000, Side::Sell));

        // Sell at 999 should cross
        assert!(book.will_cross_market(999, Side::Sell));

        // Sell at 1001 should not cross
        assert!(!book.will_cross_market(1001, Side::Sell));
    }

    #[test]
    fn test_match_market_order_partial_availability() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add an ask with only 5 units available
        let sell_id = create_order_id();
        let _ = book.add_limit_order(sell_id, 1000, 5, Side::Sell, TimeInForce::Gtc, None);

        // Try to execute a buy for 10 units
        let buy_id = create_order_id();
        let result = book.match_market_order(buy_id, 10, Side::Buy);

        // Should execute partially
        assert!(result.is_ok());
        let match_result = result.unwrap();

        // Check the match result
        assert_eq!(match_result.executed_quantity().unwrap(), Quantity::new(5));
        assert_eq!(match_result.remaining_quantity(), Quantity::new(5));
        assert!(!match_result.is_complete());

        // Ask side should be empty now
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn test_match_market_order_no_matches() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Attempt to match a market order on an empty book
        let id = create_order_id();
        let result = book.match_market_order(id, 10, Side::Buy);

        // Should return an error since there are no matching orders
        assert!(result.is_err());
        match result {
            Err(OrderBookError::InsufficientLiquidity {
                side,
                requested,
                available,
            }) => {
                assert_eq!(side, Side::Buy);
                assert_eq!(requested, 10);
                assert_eq!(available, 0);
            }
            _ => panic!("Expected InsufficientLiquidity error"),
        }
    }

    /// #259: the location carries the owner, and a fill untracks the maker
    /// by that key: other users' entries are untouched, the owner's
    /// remaining ids keep their order (#252), and the owner's entry goes
    /// once its last maker fills.
    #[test]
    fn test_fill_untracks_maker_by_location_owner() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        let alice = Hash32::new([1; 32]);
        let bob = Hash32::new([2; 32]);
        let taker = Hash32::new([3; 32]);
        for (n, owner) in [(1, alice), (2, bob), (3, alice), (4, alice)] {
            book.add_limit_order_with_user(
                Id::from_u64(n),
                100,
                5,
                Side::Sell,
                TimeInForce::Gtc,
                owner,
                None,
            )
            .unwrap();
        }
        assert_eq!(
            book.order_locations
                .get(&Id::from_u64(3))
                .map(|location| location.user_id),
            Some(alice)
        );

        // Fills alice's #1 and bob's #2 in time priority.
        book.submit_market_order_with_user(Id::from_u64(10), 10, Side::Buy, taker)
            .unwrap();
        assert_eq!(
            book.user_orders.get(&alice).map(|ids| ids.to_vec()),
            Some(vec![Id::from_u64(3), Id::from_u64(4)])
        );
        assert!(book.user_orders.get(&bob).is_none());
        assert!(book.order_locations.get(&Id::from_u64(1)).is_none());
        assert!(book.order_locations.get(&Id::from_u64(2)).is_none());

        // Fills the rest: alice's entry is removed with her last maker.
        book.submit_market_order_with_user(Id::from_u64(11), 10, Side::Buy, taker)
            .unwrap();
        assert!(book.user_orders.get(&alice).is_none());
        assert!(book.order_locations.is_empty());
    }

    /// `untrack_located_order` falls back to scanning every user's entry
    /// when the id has no location. The scan removes the id from its
    /// owner's entry only, keeps the owner's other ids in admission order,
    /// and drops an entry it empties.
    #[test]
    fn test_untrack_without_location_scans_user_entries() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        let alice = Hash32::new([1; 32]);
        let bob = Hash32::new([2; 32]);
        for raw in [1, 2, 3] {
            book.track_user_order(alice, Id::from_u64(raw));
        }
        book.track_user_order(bob, Id::from_u64(4));

        book.untrack_located_order(&Id::from_u64(2));
        assert_eq!(
            book.user_orders.get(&alice).map(|ids| ids.to_vec()),
            Some(vec![Id::from_u64(1), Id::from_u64(3)])
        );
        assert_eq!(
            book.user_orders.get(&bob).map(|ids| ids.to_vec()),
            Some(vec![Id::from_u64(4)])
        );

        book.untrack_located_order(&Id::from_u64(4));
        assert!(
            book.user_orders.get(&bob).is_none(),
            "emptied entry dropped"
        );

        // An id tracked nowhere is a no-op.
        book.untrack_located_order(&Id::from_u64(99));
        assert_eq!(book.user_orders.len(), 1);
    }
}
