//! #248: mass cancels are failure-aware and gate-safe.
//!
//! - `cancel_all_orders` holds the exclusive submit gate, so an order
//!   admitted concurrently is either cancelled and reported, or still
//!   resting and tracked: never dropped without an event.
//! - Listeners observe `cancel_all_orders` only after the book is empty.
//! - The scoped mass cancels and expiry eviction record per-order failures
//!   instead of swallowing them; a failed order stays resting and tracked
//!   (location, user index, risk, order state).
//! - Every cancelled order releases its pre-trade risk contribution.
//!
//! Per-order failures are forced through the test-only `cancel_fault_hook`:
//! pricelevel 0.10 has no public way to make a level refuse a cancel on
//! demand.

#[cfg(test)]
mod tests {
    use crate::orderbook::book::{CancelFault, OrderBook};
    use crate::orderbook::book_change_event::PriceLevelChangedEvent;
    use crate::orderbook::clock::{Clock, StubClock};
    use crate::orderbook::mass_cancel::{MassCancelFailure, MassCancelResult};
    use crate::orderbook::order_state::{CancelReason, OrderStateTracker, OrderStatus};
    use crate::orderbook::reject_reason::RejectReason;
    use crate::orderbook::risk::RiskConfig;
    use crate::{OrderBookError, OrderStateListener};
    use pricelevel::{Hash32, Id, PriceLevelError, Side, TimeInForce, TimestampMs};
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex, OnceLock};
    use std::thread;

    fn user(byte: u8) -> Hash32 {
        Hash32::new([byte; 32])
    }

    fn refused() -> PriceLevelError {
        PriceLevelError::InvalidOperation {
            message: "injected cancel refusal".to_string(),
        }
    }

    /// Book with risk (generous limits) and order-state tracking.
    fn tracked_book(symbol: &str) -> OrderBook<()> {
        let mut book = OrderBook::<()>::new(symbol);
        book.set_order_state_tracker(OrderStateTracker::with_capacity(1_000_000));
        book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(1_000_000));
        book
    }

    /// Makes every cancel of an id in `failing` be refused by its level,
    /// with nothing mutated, while `armed` is set.
    fn inject_failures(book: &mut OrderBook<()>, failing: &[u64], armed: Arc<AtomicBool>) {
        inject(book, failing, armed, || CancelFault::Refuse(refused()));
    }

    /// Makes every cancel of an id in `failing` succeed on the level and
    /// then report a failure (the level removed the order), while `armed`
    /// is set.
    fn inject_remove_then_fail(book: &mut OrderBook<()>, failing: &[u64], armed: Arc<AtomicBool>) {
        inject(book, failing, armed, || {
            CancelFault::RemoveThenFail(refused())
        });
    }

    fn inject(
        book: &mut OrderBook<()>,
        failing: &[u64],
        armed: Arc<AtomicBool>,
        fault: fn() -> CancelFault,
    ) {
        let failing: HashSet<Id> = failing.iter().copied().map(Id::from_u64).collect();
        book.cancel_fault_hook = Some(Arc::new(move |id| {
            (armed.load(Ordering::SeqCst) && failing.contains(&id)).then(fault)
        }));
    }

    fn rest(book: &OrderBook<()>, id: u64, price: u128, side: Side, owner: Hash32) {
        book.add_limit_order_with_user(
            Id::from_u64(id),
            price,
            5,
            side,
            TimeInForce::Gtc,
            owner,
            None,
        )
        .expect("rest order");
    }

    /// Every index agrees that `id` rests: location, user index, risk entry
    /// and order state.
    fn assert_resting_and_tracked(book: &OrderBook<()>, id: Id, owner: Hash32) {
        assert!(book.get_order(id).is_some(), "{id} rests");
        assert!(book.order_locations.contains_key(&id), "{id} located");
        assert!(
            book.user_orders
                .get(&owner)
                .is_some_and(|ids| ids.contains(&id)),
            "{id} in the user index"
        );
        assert!(book.risk_state.orders.contains_key(&id), "{id} in risk");
        assert_eq!(book.order_status(id), Some(OrderStatus::Open), "{id} open");
    }

    /// Every index agrees that `id` is gone, cancelled with `reason`.
    fn assert_cancelled(book: &OrderBook<()>, id: Id, reason: CancelReason) {
        assert!(book.get_order(id).is_none(), "{id} gone");
        assert!(!book.order_locations.contains_key(&id), "{id} unlocated");
        assert!(
            !book
                .user_orders
                .iter()
                .any(|entry| entry.value().contains(&id)),
            "{id} not in any user index"
        );
        assert!(!book.risk_state.orders.contains_key(&id), "{id} released");
        assert_eq!(
            book.order_status(id),
            Some(OrderStatus::Cancelled {
                filled_quantity: 0,
                reason
            }),
            "{id} cancelled"
        );
    }

    fn open_count(book: &OrderBook<()>, owner: Hash32) -> u64 {
        book.risk_state
            .counters
            .get(&owner)
            .map(|c| c.open_count.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn ids(raw: &[u64]) -> Vec<Id> {
        raw.iter().copied().map(Id::from_u64).collect()
    }

    // --- cancel_all vs concurrent admission --------------------------------

    /// Admitters race `cancel_all_orders`. Every admitted order must end
    /// either reported by exactly one cancel-all (and fully untracked) or
    /// still resting and fully tracked. Under the former shared gate an
    /// order admitted between the collection and the bulk clear vanished
    /// from both sets.
    #[test]
    fn cancel_all_vs_concurrent_admissions_loses_no_order() {
        const ADMITTERS: u64 = 4;
        const PER_ADMITTER: u64 = 1_500;

        let book = Arc::new(tracked_book("RACE"));
        let start = Arc::new(Barrier::new(usize::try_from(ADMITTERS).unwrap_or(4) + 1));
        let running = Arc::new(AtomicUsize::new(usize::try_from(ADMITTERS).unwrap_or(4)));

        let mut handles = Vec::new();
        for t in 0..ADMITTERS {
            let book = Arc::clone(&book);
            let start = Arc::clone(&start);
            let running = Arc::clone(&running);
            handles.push(thread::spawn(move || {
                let owner = user(u8::try_from(t + 1).unwrap_or(1));
                let mut admitted = Vec::new();
                start.wait();
                for n in 0..PER_ADMITTER {
                    let id = Id::from_u64(t * 1_000_000 + n + 1);
                    // Bids below asks: nothing ever crosses.
                    let (side, price) = if n % 2 == 0 {
                        (Side::Buy, 90 + u128::from(n % 10))
                    } else {
                        (Side::Sell, 110 + u128::from(n % 10))
                    };
                    book.add_limit_order_with_user(
                        id,
                        price,
                        1,
                        side,
                        TimeInForce::Gtc,
                        owner,
                        None,
                    )
                    .expect("admission");
                    admitted.push((id, owner));
                }
                running.fetch_sub(1, Ordering::SeqCst);
                admitted
            }));
        }

        start.wait();
        let mut reported: Vec<Id> = Vec::new();
        let mut rounds = 0usize;
        while running.load(Ordering::SeqCst) > 0 {
            let result = book.cancel_all_orders();
            assert!(!result.has_failures());
            assert_eq!(result.cancelled_count(), result.cancelled_order_ids().len());
            reported.extend_from_slice(result.cancelled_order_ids());
            rounds += 1;
            thread::yield_now();
        }
        let admitted: Vec<(Id, Hash32)> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("admitter"))
            .collect();
        assert!(rounds > 0);

        let reported_set: HashSet<Id> = reported.iter().copied().collect();
        assert_eq!(reported_set.len(), reported.len(), "no id reported twice");

        let mut resting_per_owner: HashMap<Hash32, usize> = HashMap::new();
        for &(id, owner) in &admitted {
            if reported_set.contains(&id) {
                assert_cancelled(&book, id, CancelReason::MassCancelAll);
            } else {
                assert_resting_and_tracked(&book, id, owner);
                *resting_per_owner.entry(owner).or_default() += 1;
            }
        }
        assert_eq!(
            reported_set.len() + resting_per_owner.values().sum::<usize>(),
            admitted.len(),
            "every admitted order is accounted for"
        );
        // Risk counters agree with what still rests.
        for t in 0..ADMITTERS {
            let owner = user(u8::try_from(t + 1).unwrap_or(1));
            let resting = resting_per_owner.get(&owner).copied().unwrap_or(0);
            assert_eq!(
                open_count(&book, owner),
                u64::try_from(resting).expect("count fits u64")
            );
        }
        // Nothing else rests.
        assert_eq!(
            book.order_locations.len(),
            admitted.len() - reported_set.len()
        );
    }

    /// The level and order-state listeners run after the bulk clear: during
    /// every event the book is already empty and untracked.
    #[test]
    fn cancel_all_emits_after_the_book_is_cleared() {
        let book_cell: Arc<OnceLock<std::sync::Weak<OrderBook<()>>>> = Arc::new(OnceLock::new());
        let observations: Arc<Mutex<Vec<(bool, usize, usize)>>> = Arc::new(Mutex::new(Vec::new()));

        let mut book = OrderBook::<()>::new("EMIT");
        let cell = Arc::clone(&book_cell);
        let seen = Arc::clone(&observations);
        book.set_price_level_listener(Arc::new(move |ev: PriceLevelChangedEvent| {
            if ev.quantity != 0 {
                return;
            }
            if let Some(book) = cell.get().and_then(std::sync::Weak::upgrade) {
                seen.lock().expect("obs").push((
                    book.best_bid().is_none() && book.best_ask().is_none(),
                    book.order_locations.len(),
                    book.user_orders.len(),
                ));
            }
        }));
        let state_cell = Arc::clone(&book_cell);
        let state_seen = Arc::new(AtomicUsize::new(0));
        let state_counter = Arc::clone(&state_seen);
        let listener: OrderStateListener =
            Arc::new(move |id, _old: &OrderStatus, new: &OrderStatus| {
                if matches!(
                    new,
                    OrderStatus::Cancelled {
                        reason: CancelReason::MassCancelAll,
                        ..
                    }
                ) && let Some(book) = state_cell.get().and_then(std::sync::Weak::upgrade)
                {
                    assert!(book.get_order(id).is_none(), "state event after removal");
                    assert!(!book.order_locations.contains_key(&id));
                    state_counter.fetch_add(1, Ordering::SeqCst);
                }
            });
        let mut tracker = OrderStateTracker::new();
        tracker.set_listener(listener);
        book.set_order_state_tracker(tracker);

        let book = Arc::new(book);
        assert!(book_cell.set(Arc::downgrade(&book)).is_ok());
        for (id, price, side) in [
            (1, 100, Side::Buy),
            (2, 100, Side::Buy),
            (3, 99, Side::Buy),
            (4, 110, Side::Sell),
        ] {
            rest(&book, id, price, side, user(1));
        }
        let result = book.cancel_all_orders();
        // Bids ascending (99 then 100, oldest first), then asks.
        assert_eq!(result.cancelled_order_ids(), ids(&[3, 1, 2, 4]).as_slice());

        let obs = observations.lock().expect("obs");
        assert_eq!(obs.len(), 3, "one event per cleared level");
        assert!(
            obs.iter()
                .all(|&(empty, locs, users)| empty && locs == 0 && users == 0),
            "listener saw a book that was not cleared yet: {obs:?}"
        );
        assert_eq!(state_seen.load(Ordering::SeqCst), 4);
    }

    // --- per-order failures -------------------------------------------------

    /// A by-user cancel whose middle order fails: the other two are
    /// cancelled and reported, the failed one keeps its user-index entry and
    /// everything else, and a retry cancels it and drops the entry.
    #[test]
    fn cancel_by_user_partial_failure_keeps_the_failed_order_indexed() {
        let owner = user(1);
        let other = user(2);
        let armed = Arc::new(AtomicBool::new(true));
        let mut book = tracked_book("USER");
        inject_failures(&mut book, &[2], Arc::clone(&armed));
        rest(&book, 1, 100, Side::Buy, owner);
        rest(&book, 2, 101, Side::Buy, owner);
        rest(&book, 3, 120, Side::Sell, owner);
        rest(&book, 4, 100, Side::Buy, other);

        let result = book.cancel_orders_by_user(owner);
        assert_eq!(result.cancelled_order_ids(), ids(&[1, 3]).as_slice());
        assert_eq!(result.cancelled_count(), 2);
        assert!(result.has_failures());
        assert!(!result.is_refused());
        assert_eq!(
            result.failures(),
            &[MassCancelFailure::OrderCancelFailed {
                order_id: Id::from_u64(2),
                error: refused(),
            }]
        );

        assert_cancelled(&book, Id::from_u64(1), CancelReason::MassCancelByUser);
        assert_cancelled(&book, Id::from_u64(3), CancelReason::MassCancelByUser);
        assert_resting_and_tracked(&book, Id::from_u64(2), owner);
        assert_eq!(
            book.user_orders.get(&owner).map(|ids| ids.to_vec()),
            Some(ids(&[2]))
        );
        assert_eq!(open_count(&book, owner), 1, "only the failed order counts");
        assert_resting_and_tracked(&book, Id::from_u64(4), other);

        armed.store(false, Ordering::SeqCst);
        let retry = book.cancel_orders_by_user(owner);
        assert_eq!(retry.cancelled_order_ids(), ids(&[2]).as_slice());
        assert!(!retry.has_failures());
        assert_cancelled(&book, Id::from_u64(2), CancelReason::MassCancelByUser);
        assert!(book.user_orders.get(&owner).is_none(), "entry dropped");
        assert_eq!(open_count(&book, owner), 0);
        assert_resting_and_tracked(&book, Id::from_u64(4), other);
    }

    /// A stale user-index id (no longer resting) is dropped by a by-user
    /// cancel, as the pre-#248 whole-entry removal did.
    #[test]
    fn cancel_by_user_drops_stale_index_ids() {
        let owner = user(1);
        let book = tracked_book("STALE");
        rest(&book, 1, 100, Side::Buy, owner);
        book.track_user_order(owner, Id::from_u64(77));

        let result = book.cancel_orders_by_user(owner);
        assert_eq!(result.cancelled_order_ids(), ids(&[1]).as_slice());
        assert!(!result.has_failures());
        assert!(book.user_orders.get(&owner).is_none());
    }

    /// By-side and by-range record failures in the deterministic traversal
    /// order (ascending price, then insertion sequence), interleaved with
    /// the successes exactly as walked.
    #[test]
    fn scoped_cancels_record_failures_in_traversal_order() {
        let owner = user(1);
        let armed = Arc::new(AtomicBool::new(true));
        let mut book = tracked_book("SCOPE");
        inject_failures(&mut book, &[2, 5], Arc::clone(&armed));
        // Bids: 100 -> [1, 2], 101 -> [3]; asks: 120 -> [4, 5], 121 -> [6].
        rest(&book, 1, 100, Side::Buy, owner);
        rest(&book, 2, 100, Side::Buy, owner);
        rest(&book, 3, 101, Side::Buy, owner);
        rest(&book, 4, 120, Side::Sell, owner);
        rest(&book, 5, 120, Side::Sell, owner);
        rest(&book, 6, 121, Side::Sell, owner);

        let by_side = book.cancel_orders_by_side(Side::Buy);
        assert_eq!(by_side.cancelled_order_ids(), ids(&[1, 3]).as_slice());
        assert_eq!(
            by_side.failures(),
            &[MassCancelFailure::OrderCancelFailed {
                order_id: Id::from_u64(2),
                error: refused(),
            }]
        );
        assert_resting_and_tracked(&book, Id::from_u64(2), owner);
        assert_eq!(book.best_bid(), Some(100), "the failed bid still quotes");

        let by_range = book.cancel_orders_by_price_range(Side::Sell, 120, 121);
        assert_eq!(by_range.cancelled_order_ids(), ids(&[4, 6]).as_slice());
        assert_eq!(
            by_range.failures(),
            &[MassCancelFailure::OrderCancelFailed {
                order_id: Id::from_u64(5),
                error: refused(),
            }]
        );
        assert_resting_and_tracked(&book, Id::from_u64(5), owner);
        assert_cancelled(&book, Id::from_u64(4), CancelReason::MassCancelByPriceRange);
        assert_cancelled(&book, Id::from_u64(6), CancelReason::MassCancelByPriceRange);
        assert_eq!(open_count(&book, owner), 2);

        // Same book shape, same failures: byte-identical outcome.
        let json = serde_json::to_string(&by_range).expect("serialize");
        let again: MassCancelResult = serde_json::from_str(&json).expect("decode");
        assert_eq!(again.failures(), by_range.failures());
    }

    /// A single `cancel_order` whose level refuses the removal returns the
    /// typed error (not `Ok(None)`) and leaves the order fully tracked.
    #[test]
    fn cancel_order_surfaces_a_level_refusal() {
        let owner = user(1);
        let armed = Arc::new(AtomicBool::new(true));
        let mut book = tracked_book("ONE");
        inject_failures(&mut book, &[1], Arc::clone(&armed));
        rest(&book, 1, 100, Side::Buy, owner);

        let err = book.cancel_order(Id::from_u64(1)).expect_err("refused");
        assert!(matches!(
            err,
            OrderBookError::PriceLevelError(PriceLevelError::InvalidOperation { .. })
        ));
        assert_resting_and_tracked(&book, Id::from_u64(1), owner);
        assert_eq!(book.cancel_order(Id::from_u64(9)).ok(), Some(None));
    }

    fn gtd_book(symbol: &str) -> OrderBook<()> {
        let mut book = OrderBook::<()>::with_clock(
            symbol,
            Arc::new(StubClock::starting_at(0)) as Arc<dyn Clock>,
        );
        book.set_order_state_tracker(OrderStateTracker::new());
        book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(100));
        book
    }

    fn rest_gtd(book: &OrderBook<()>, id: u64, price: u128, owner: Hash32) {
        book.add_limit_order_with_user(
            Id::from_u64(id),
            price,
            5,
            Side::Buy,
            TimeInForce::Gtd(1_000),
            owner,
            None,
        )
        .expect("gtd");
    }

    /// Eviction carries on past a failed order and evicts the rest; the
    /// result names the evicted orders and the failure, the failed order
    /// stays resting, and a later sweep evicts it.
    #[test]
    fn evict_expired_reports_a_partial_sweep() {
        let owner = user(1);
        let armed = Arc::new(AtomicBool::new(true));
        let mut book = gtd_book("EVICT");
        inject_failures(&mut book, &[2, 3], Arc::clone(&armed));
        for (id, price) in [(1, 100), (2, 101), (3, 102), (4, 103)] {
            rest_gtd(&book, id, price, owner);
        }

        let swept = book
            .evict_expired_orders(TimestampMs::new(1_000))
            .expect("sweep ran");
        assert_eq!(swept.evicted_order_ids(), ids(&[1, 4]).as_slice());
        let bodies: Vec<Id> = swept.iter().map(|o| o.id()).collect();
        assert_eq!(bodies, ids(&[1, 4]));
        assert_eq!(swept.len(), 2);
        assert!(swept.has_failures());
        assert_eq!(
            swept.failures(),
            &[
                MassCancelFailure::OrderCancelFailed {
                    order_id: Id::from_u64(2),
                    error: refused(),
                },
                MassCancelFailure::OrderCancelFailed {
                    order_id: Id::from_u64(3),
                    error: refused(),
                },
            ]
        );
        let failed: Vec<Id> = swept.mass_cancel_result().failed_order_ids().collect();
        assert_eq!(failed, ids(&[2, 3]));
        assert_cancelled(&book, Id::from_u64(1), CancelReason::TimeInForceExpired);
        assert_cancelled(&book, Id::from_u64(4), CancelReason::TimeInForceExpired);
        assert_resting_and_tracked(&book, Id::from_u64(2), owner);
        assert_resting_and_tracked(&book, Id::from_u64(3), owner);
        assert_eq!(open_count(&book, owner), 2);

        armed.store(false, Ordering::SeqCst);
        let retry = book
            .evict_expired_orders(TimestampMs::new(1_000))
            .expect("retry");
        assert_eq!(retry.evicted_order_ids(), ids(&[2, 3]).as_slice());
        assert!(!retry.has_failures());
        assert_eq!(open_count(&book, owner), 0);
    }

    // --- level removes the order, then fails ---------------------------------

    /// A level that removes the order and then reports a failure: the book
    /// completes the removal (no stale location, user index, risk or state
    /// entry, empty level dropped) and `cancel_order` reports the fault.
    #[test]
    fn cancel_order_completes_a_removal_the_level_committed() {
        let owner = user(1);
        let events: Arc<Mutex<Vec<PriceLevelChangedEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let armed = Arc::new(AtomicBool::new(true));
        let mut book = tracked_book("FAULT");
        book.set_price_level_listener(Arc::new(move |ev: PriceLevelChangedEvent| {
            sink.lock().expect("events").push(ev);
        }));
        inject_remove_then_fail(&mut book, &[1], Arc::clone(&armed));
        rest(&book, 1, 100, Side::Buy, owner);
        rest(&book, 2, 99, Side::Buy, owner);
        events.lock().expect("events").clear();

        let err = book.cancel_order(Id::from_u64(1)).expect_err("fault");
        match &err {
            OrderBookError::OrderRemovedWithLevelFault { order_id, source } => {
                assert_eq!(*order_id, Id::from_u64(1));
                assert_eq!(source.as_ref(), &refused());
            }
            other => panic!("expected OrderRemovedWithLevelFault, got {other:?}"),
        }
        assert_eq!(RejectReason::from(&err), RejectReason::Other(0));
        assert!(std::error::Error::source(&err).is_some());
        assert_cancelled(&book, Id::from_u64(1), CancelReason::UserRequested);
        assert_eq!(book.best_bid(), Some(99), "the emptied level is gone");
        assert_eq!(open_count(&book, owner), 1);
        let events = events.lock().expect("events");
        assert_eq!(events.len(), 1, "one level event for the removal");
        assert_eq!(events[0].price, 100);
        assert_eq!(events[0].quantity, 0);
        drop(events);

        // A later mass cancel does not trip over a stale entry.
        let all = book.cancel_orders_by_user(owner);
        assert_eq!(all.cancelled_order_ids(), ids(&[2]).as_slice());
        assert!(!all.has_failures());
    }

    /// A mass cancel lists an order whose level removed it and then failed
    /// as cancelled, and also records the fault.
    #[test]
    fn mass_cancel_reports_a_removal_the_level_committed() {
        let owner = user(1);
        let armed = Arc::new(AtomicBool::new(true));
        let mut book = tracked_book("FAULTS");
        inject_remove_then_fail(&mut book, &[2], Arc::clone(&armed));
        rest(&book, 1, 100, Side::Buy, owner);
        rest(&book, 2, 100, Side::Buy, owner);
        rest(&book, 3, 101, Side::Buy, owner);

        let result = book.cancel_orders_by_side(Side::Buy);
        assert_eq!(result.cancelled_order_ids(), ids(&[1, 2, 3]).as_slice());
        assert_eq!(
            result.failures(),
            &[MassCancelFailure::LevelFaultAfterRemoval {
                order_id: Id::from_u64(2),
                error: refused(),
            }]
        );
        assert!(!result.is_refused());
        assert_eq!(result.failed_order_ids().count(), 0);
        assert!(matches!(
            result.failures()[0].to_order_book_error(),
            OrderBookError::OrderRemovedWithLevelFault { .. }
        ));
        for id in [1, 2, 3] {
            assert_cancelled(&book, Id::from_u64(id), CancelReason::MassCancelBySide);
        }
        assert_eq!(open_count(&book, owner), 0);
        assert!(book.user_orders.is_empty());
        assert_eq!(book.best_bid(), None);
    }

    // --- risk release -------------------------------------------------------

    /// Every mass cancel path releases the risk contribution of each order
    /// it cancels: an account at its open-order limit can admit a full
    /// allowance again afterwards.
    #[test]
    fn every_mass_cancel_releases_risk_for_each_cancelled_order() {
        const LIMIT: u64 = 3;
        let owner = user(1);
        let mut book = OrderBook::<()>::new("RISK");
        book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(LIMIT));
        let next = AtomicUsize::new(1);
        let fill_to_limit = |book: &OrderBook<()>| {
            for k in 0..LIMIT {
                let id = u64::try_from(next.fetch_add(1, Ordering::SeqCst)).unwrap_or(0);
                rest(book, id, 100 + u128::from(k), Side::Buy, owner);
            }
            assert!(matches!(
                book.add_limit_order_with_user(
                    Id::from_u64(999_999),
                    100,
                    5,
                    Side::Buy,
                    TimeInForce::Gtc,
                    owner,
                    None,
                ),
                Err(OrderBookError::RiskMaxOpenOrders { .. })
            ));
        };
        let released = |book: &OrderBook<()>, result: &MassCancelResult| {
            assert_eq!(
                result.cancelled_count(),
                usize::try_from(LIMIT).unwrap_or(0)
            );
            assert!(!result.has_failures());
            assert!(book.risk_state.orders.is_empty());
            assert_eq!(open_count(book, owner), 0);
        };

        fill_to_limit(&book);
        released(&book, &book.cancel_all_orders());
        fill_to_limit(&book);
        released(&book, &book.cancel_orders_by_side(Side::Buy));
        fill_to_limit(&book);
        released(&book, &book.cancel_orders_by_user(owner));
        fill_to_limit(&book);
        released(
            &book,
            &book.cancel_orders_by_price_range(Side::Buy, 0, 1_000),
        );
        fill_to_limit(&book);
    }

    /// A failed cancel keeps its risk contribution: the account's allowance
    /// only grows by the orders actually cancelled.
    #[test]
    fn failed_cancel_keeps_its_risk_contribution() {
        let owner = user(1);
        let armed = Arc::new(AtomicBool::new(true));
        let mut book = OrderBook::<()>::new("RISKF");
        book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(3));
        inject_failures(&mut book, &[2], Arc::clone(&armed));
        for id in 1..=3 {
            rest(&book, id, 100, Side::Buy, owner);
        }
        let result = book.cancel_orders_by_user(owner);
        assert_eq!(result.cancelled_count(), 2);
        assert_eq!(open_count(&book, owner), 1);
        rest(&book, 10, 100, Side::Buy, owner);
        rest(&book, 11, 100, Side::Buy, owner);
        assert!(matches!(
            book.add_limit_order_with_user(
                Id::from_u64(12),
                100,
                5,
                Side::Buy,
                TimeInForce::Gtc,
                owner,
                None,
            ),
            Err(OrderBookError::RiskMaxOpenOrders { .. })
        ));
    }
}
