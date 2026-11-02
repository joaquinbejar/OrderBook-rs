//! #288: concurrent crossing adds must never leave an order indexed but
//! not resting (or resting but not indexed).
//!
//! Several threads share one `STPMode::None` book (shared submit gate, no
//! listeners) and mix passive adds, crossing adds that sweep and rest a
//! residual, and cancels. After the run every index the book keeps for a
//! resting order must agree with the price levels: `order_locations`, the
//! `user_orders` index, the per-order risk entries and per-account risk
//! counters, and the order-state tracker.

#[cfg(test)]
// tests may panic: rules/global_rules.md § Testing
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use crate::orderbook::book::{OrderBook, OrderLocation};
    use crate::orderbook::order_state::{OrderStateTracker, OrderStatus};
    use crate::orderbook::risk::RiskConfig;
    use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::Duration;

    const THREADS: u64 = 8;
    const OPS_PER_THREAD: u64 = 400;
    /// Independent books per test run, each driven by the full thread mix.
    const SCENARIOS: u64 = 25;
    const ID_STRIDE: u64 = 1_000_000;

    /// Deterministic xorshift64 per worker (the test must not depend on a
    /// random seed to be meaningful; the interleaving provides the noise).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    fn user(worker: u64) -> Hash32 {
        let byte = u8::try_from(worker % 4 + 1).expect("small");
        Hash32::new([byte; 32])
    }

    fn order(raw: u64, worker: u64, side: Side, price: u128, qty: u64) -> OrderType<()> {
        OrderType::Standard {
            id: Id::from_u64(raw),
            price: Price::new(price),
            quantity: Quantity::new(qty),
            side,
            time_in_force: TimeInForce::Gtc,
            user_id: user(worker),
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        }
    }

    fn book() -> OrderBook<()> {
        let mut book = OrderBook::<()>::new("RACE-288");
        // Limits far above anything the workload reaches: the config only
        // turns on per-order risk bookkeeping so it can be checked.
        book.set_risk_config(
            RiskConfig::new()
                .with_max_open_orders_per_account(1_000_000)
                .with_max_notional_per_account(u128::MAX / 4),
        );
        book.set_order_state_tracker(OrderStateTracker::with_capacity(1_000_000));
        book
    }

    fn run_workers(book: &Arc<OrderBook<()>>, seed: u64) {
        let barrier = Arc::new(Barrier::new(usize::try_from(THREADS).expect("small")));
        let mut workers = Vec::new();
        for worker in 0..THREADS {
            let book = Arc::clone(book);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                let mut rng = Rng(seed
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    .wrapping_add(worker.wrapping_add(1).wrapping_mul(0xD1B5_4A32_D192_ED03))
                    | 1);
                let base = (worker + 1) * ID_STRIDE;
                barrier.wait();
                for op in 0..OPS_PER_THREAD {
                    let raw = base + op;
                    let side = if rng.below(2) == 0 {
                        Side::Buy
                    } else {
                        Side::Sell
                    };
                    let qty = rng.below(5) + 1;
                    match rng.below(10) {
                        // Passive add one tick either side of the touch.
                        0..=3 => {
                            let price = match side {
                                Side::Buy => 99 - u128::from(rng.below(2)),
                                Side::Sell => 101 + u128::from(rng.below(2)),
                            };
                            let _ = book.add_order(order(raw, worker, side, price, qty));
                        }
                        // Crossing add: sweeps the opposite side and rests
                        // its residual deep inside the spread, where the
                        // other side's crossing adds sweep it again.
                        4..=7 => {
                            let price = match side {
                                Side::Buy => 101 + u128::from(rng.below(2)),
                                Side::Sell => 99 - u128::from(rng.below(2)),
                            };
                            let _ = book.add_order(order(raw, worker, side, price, qty + 2));
                        }
                        // Cancel one of this worker's earlier ids (it may
                        // already be filled or cancelled).
                        _ => {
                            if op > 0 {
                                let target = base + rng.below(op);
                                let _ = book.cancel_order(Id::from_u64(target));
                            }
                        }
                    }
                }
            }));
        }
        for worker in workers {
            worker.join().expect("worker");
        }
    }

    /// Every index the book keeps for resting orders agrees with the price
    /// levels, in both directions.
    fn assert_book_consistent(book: &OrderBook<()>) {
        // Ground truth: what actually rests on the levels.
        let mut resting: HashMap<Id, (u128, Side, Hash32, u64)> = HashMap::new();
        for (side, levels) in [(Side::Buy, &book.bids), (Side::Sell, &book.asks)] {
            for entry in levels.iter() {
                for order in entry.value().iter_orders() {
                    let previous = resting.insert(
                        order.id(),
                        (
                            *entry.key(),
                            side,
                            order.user_id(),
                            order.visible_quantity().as_u64(),
                        ),
                    );
                    assert!(previous.is_none(), "{} rests twice", order.id());
                }
            }
        }

        // order_locations <-> levels.
        for location in book.order_locations.iter() {
            let OrderLocation {
                price,
                side,
                user_id,
                ..
            } = *location.value();
            let rests = resting.get(location.key());
            assert!(
                rests.is_some_and(|r| r.0 == price && r.1 == side && r.2 == user_id),
                "order {} is indexed at {side} {price} for {user_id:?} but does not rest there (rests: {rests:?})",
                location.key()
            );
        }
        for (id, (price, side, owner, _)) in &resting {
            assert_eq!(
                book.order_locations
                    .get(id)
                    .map(|loc| (loc.price, loc.side, loc.user_id)),
                Some((*price, *side, *owner)),
                "order {id} rests but is not indexed"
            );
        }

        // user_orders <-> levels: each resting id exactly once, under its owner.
        let mut seen: HashSet<Id> = HashSet::new();
        for entry in book.user_orders.iter() {
            assert!(!entry.value().is_empty(), "empty user entry kept");
            for id in entry.value().to_vec() {
                assert!(seen.insert(id), "order {id} is in the user index twice");
                let rests = resting.get(&id);
                assert!(
                    rests.is_some_and(|r| r.2 == *entry.key()),
                    "order {id} is in the user index but does not rest (rests: {rests:?})"
                );
            }
        }
        assert_eq!(seen.len(), resting.len(), "user index and level counts");

        // Risk entries <-> levels, and the per-account counters.
        if book.risk_config().is_none() {
            assert!(book.risk_state.orders.is_empty() && book.risk_state.counters.is_empty());
        } else {
            assert_risk_consistent(book, &resting);
        }
        assert_states_consistent(book, &resting);
    }

    fn assert_risk_consistent(
        book: &OrderBook<()>,
        resting: &HashMap<Id, (u128, Side, Hash32, u64)>,
    ) {
        let mut per_account: HashMap<Hash32, (u64, u128)> = HashMap::new();
        for (id, (price, _, account, qty)) in resting {
            let entry = book
                .risk_state
                .orders
                .get(id)
                .unwrap_or_else(|| panic!("order {id} rests without a risk entry"));
            assert_eq!(entry.account, *account, "risk account of {id}");
            assert_eq!(entry.price, *price, "risk price of {id}");
            assert_eq!(entry.remaining_qty, *qty, "risk remaining of {id}");
            let slot = per_account.entry(*account).or_default();
            slot.0 += 1;
            slot.1 += u128::from(*qty) * *price;
        }
        for entry in book.risk_state.orders.iter() {
            assert!(
                resting.contains_key(entry.key()),
                "risk entry for {} whose order does not rest",
                entry.key()
            );
        }
        for counters in book.risk_state.counters.iter() {
            let expected = per_account.get(counters.key()).copied().unwrap_or((0, 0));
            assert_eq!(
                (
                    counters.open_count.load(Ordering::SeqCst),
                    counters.resting_notional.load()
                ),
                expected,
                "risk counters of an account (anomalies {})",
                book.risk_accounting_anomalies()
            );
        }
        // Reverse pass: every account with resting orders has its counters
        // (an eviction racing an admission must not drop a live account).
        for (account, expected) in &per_account {
            let counters = book
                .risk_state
                .counters
                .get(account)
                .unwrap_or_else(|| panic!("account {account} rests orders without counters"));
            assert_eq!(
                (
                    counters.open_count.load(Ordering::SeqCst),
                    counters.resting_notional.load()
                ),
                *expected,
                "risk counters of account {account}"
            );
        }
        assert_eq!(
            book.risk_accounting_anomalies(),
            0,
            "no risk accounting anomaly"
        );
    }

    fn assert_states_consistent(
        book: &OrderBook<()>,
        resting: &HashMap<Id, (u128, Side, Hash32, u64)>,
    ) {
        // Order states: resting orders are active, and every active state
        // belongs to a resting order.
        let tracker = book.order_state_tracker().expect("tracker installed");
        for id in resting.keys() {
            let status = tracker.get(*id);
            assert!(
                matches!(
                    status,
                    Some(OrderStatus::Open | OrderStatus::PartiallyFilled { .. })
                ),
                "resting order {id} has state {status:?}"
            );
        }
        assert_eq!(
            tracker.active_count(),
            resting.len(),
            "active order states and resting orders"
        );
    }

    /// Hung-test detector only: every rendezvous is expected to complete
    /// immediately, so a timeout means a deadlock or a hook that was never
    /// reached, never a slow machine taking a different branch.
    const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(10);

    /// Parks the thread resting order `parked` right after its level
    /// admitted it (the order is matchable from then on), runs `during` on
    /// the calling thread, then lets the resting thread finish. Channel
    /// rendezvous with a timeout, so a regression that never reaches the
    /// hook fails instead of hanging the suite.
    fn with_rest_parked(
        mut book: OrderBook<()>,
        parked: OrderType<()>,
        during: impl FnOnce(&OrderBook<()>),
    ) -> Arc<OrderBook<()>> {
        let parked_id = parked.id();
        let (admitted_tx, admitted_rx) = channel::<()>();
        let (resume_tx, resume_rx) = channel::<()>();
        {
            let admitted_tx = Mutex::new(admitted_tx);
            let resume_rx = Mutex::new(resume_rx);
            // Parks the first admission of `parked_id` only, so a later
            // same-id admission passes straight through.
            let fired = AtomicBool::new(false);
            book.rest_interleave_hook = Some(Arc::new(move |order_id: Id| {
                if order_id == parked_id && !fired.swap(true, Ordering::SeqCst) {
                    admitted_tx
                        .lock()
                        .expect("admitted sender")
                        .send(())
                        .expect("the test thread is waiting");
                    resume_rx
                        .lock()
                        .expect("resume receiver")
                        .recv_timeout(RENDEZVOUS_TIMEOUT)
                        .expect("resumed by the test thread");
                }
            }));
        }
        let book = Arc::new(book);
        let rester = {
            let book = Arc::clone(&book);
            thread::spawn(move || book.add_order(parked))
        };
        admitted_rx
            .recv_timeout(RENDEZVOUS_TIMEOUT)
            .expect("the parked order reached the hook");
        during(&book);
        resume_tx.send(()).expect("the rester is parked");
        rester
            .join()
            .expect("rester")
            .expect("the parked order rested");
        book
    }

    /// Deterministic #288 window: a sweep fully consumes an order between
    /// its level admission and the rest of its bookkeeping. The order must
    /// end with no location, no user-index or risk entry, and `Filled` as
    /// its last state.
    #[test]
    fn test_sweep_consuming_an_order_mid_rest_leaves_no_index() {
        let maker = Id::from_u64(1);
        let book = with_rest_parked(book(), order(1, 0, Side::Sell, 100, 3), |book| {
            let taker = book
                .add_order(order(2, 1, Side::Buy, 100, 3))
                .expect("taker fills");
            assert_eq!(taker.id(), Id::from_u64(2));
        });
        assert!(book.order_locations.get(&maker).is_none(), "no location");
        assert!(book.get_order(maker).is_none(), "not resting");
        assert!(
            book.risk_state.orders.get(&maker).is_none(),
            "no risk entry"
        );
        assert_eq!(
            book.order_status(maker),
            Some(OrderStatus::Filled { filled_quantity: 3 })
        );
        assert!(book.asks.is_empty() && book.bids.is_empty());
        assert_book_consistent(&book);
    }

    /// A partial fill inside the same window leaves the order resting,
    /// fully indexed, with the reduced quantity booked.
    #[test]
    fn test_partial_fill_mid_rest_keeps_the_order_indexed() {
        let maker = Id::from_u64(1);
        let book = with_rest_parked(book(), order(1, 0, Side::Sell, 100, 5), |book| {
            book.add_order(order(2, 1, Side::Buy, 100, 2))
                .expect("taker fills");
        });
        assert_eq!(
            book.get_order(maker).map(|o| o.visible_quantity().as_u64()),
            Some(3)
        );
        assert_eq!(
            book.risk_state.orders.get(&maker).map(|e| e.remaining_qty),
            Some(3)
        );
        assert_book_consistent(&book);
    }

    /// A cancel inside the window finds the order (it is located before it
    /// becomes matchable) and removes it completely.
    #[test]
    fn test_cancel_mid_rest_removes_the_order() {
        let maker = Id::from_u64(1);
        let book = with_rest_parked(book(), order(1, 0, Side::Sell, 100, 3), |book| {
            let cancelled = book.cancel_order(maker).expect("cancel");
            assert!(cancelled.is_some(), "the resting order is cancellable");
        });
        assert!(book.order_locations.get(&maker).is_none(), "no location");
        assert!(
            book.risk_state.orders.get(&maker).is_none(),
            "no risk entry"
        );
        assert!(matches!(
            book.order_status(maker),
            Some(OrderStatus::Cancelled { .. })
        ));
        assert!(book.asks.is_empty(), "the emptied level was removed");
        assert_book_consistent(&book);
    }

    /// Copilot on #290: the id is reused while the first admission is
    /// still parked. A sweep consumes the parked order, a new order with the
    /// same id (another user, same price and side) rests, then the parked
    /// thread finishes. The user index must hold the id exactly once, under
    /// the new order's user.
    #[test]
    fn test_id_reused_mid_rest_keeps_one_user_entry() {
        let reused = Id::from_u64(1);
        let book = with_rest_parked(book(), order(1, 0, Side::Sell, 100, 3), |book| {
            book.add_order(order(2, 1, Side::Buy, 100, 3))
                .expect("the sweep consumes the parked order");
            assert!(book.order_locations.get(&reused).is_none(), "id released");
            book.add_order(order(1, 2, Side::Sell, 100, 4))
                .expect("the freed id is reusable");
        });
        let entries: Vec<(Hash32, usize)> = book
            .user_orders
            .iter()
            .map(|entry| (*entry.key(), usize::from(entry.value().contains(&reused))))
            .filter(|(_, count)| *count > 0)
            .collect();
        assert_eq!(entries, vec![(user(2), 1)], "one entry, the new owner's");
        assert_eq!(
            book.get_order(reused)
                .map(|o| o.visible_quantity().as_u64()),
            Some(4)
        );
        assert_book_consistent(&book);
    }

    /// A second order with the same id arriving inside the window is
    /// refused: the first owns the location, and neither index is
    /// overwritten.
    #[test]
    fn test_same_id_admission_mid_rest_is_refused() {
        // No risk config: its per-order entry would refuse the duplicate on
        // its own; the location claim must do it without one.
        let mut plain = OrderBook::<()>::new("RACE-288-DUP");
        plain.set_order_state_tracker(OrderStateTracker::with_capacity(1_000));
        let book = with_rest_parked(plain, order(1, 0, Side::Sell, 100, 3), |book| {
            let duplicate = book.add_order(order(1, 1, Side::Sell, 102, 4));
            assert!(matches!(
                duplicate,
                Err(crate::orderbook::OrderBookError::DuplicateOrderId { .. })
            ));
        });
        assert_eq!(
            book.order_locations
                .get(&Id::from_u64(1))
                .map(|l| l.price_side()),
            Some((100, Side::Sell))
        );
        assert_book_consistent(&book);
    }

    /// #311: the user index is updated before the location claim, so an
    /// admission whose claim is refused has already appended a slot and
    /// must take out exactly that one. Admission A of id 1 is parked just
    /// before its claim (the pre-claim risk hook); admission B of the same
    /// id and owner rests in the meantime; A then resumes, appends its own
    /// slot to the shared entry, loses the claim and untracks by its
    /// sequence. B's slot, the one its location points at, must survive.
    #[test]
    fn test_refused_claim_untracks_only_its_own_slot() {
        // No risk config: its per-order entry would refuse A on its own,
        // before the claim this test is about.
        let mut book = OrderBook::<()>::new("RACE-311");
        book.set_order_state_tracker(OrderStateTracker::with_capacity(1_000));
        let reused = Id::from_u64(1);
        let (parked_tx, parked_rx) = channel::<()>();
        let (resume_tx, resume_rx) = channel::<()>();
        {
            let parked_tx = Mutex::new(parked_tx);
            let resume_rx = Mutex::new(resume_rx);
            // Parks the first admission of the id only; B passes through.
            let fired = AtomicBool::new(false);
            book.rest_risk_fault_hook = Some(Arc::new(move |order_id: Id| {
                if order_id == reused && !fired.swap(true, Ordering::SeqCst) {
                    parked_tx
                        .lock()
                        .expect("parked sender")
                        .send(())
                        .expect("the test thread is waiting");
                    resume_rx
                        .lock()
                        .expect("resume receiver")
                        .recv_timeout(RENDEZVOUS_TIMEOUT)
                        .expect("resumed by the test thread");
                }
                None
            }));
        }
        let book = Arc::new(book);
        let first = {
            let book = Arc::clone(&book);
            thread::spawn(move || book.add_order(order(1, 0, Side::Sell, 102, 4)))
        };
        parked_rx
            .recv_timeout(RENDEZVOUS_TIMEOUT)
            .expect("A reached the pre-claim hook");
        book.add_order(order(1, 0, Side::Sell, 100, 3))
            .expect("B rests while A is parked");
        resume_tx.send(()).expect("A is parked");
        let refused = first.join().expect("A's thread");
        assert!(matches!(
            refused,
            Err(crate::orderbook::OrderBookError::DuplicateOrderId { .. })
        ));

        assert_eq!(
            book.user_orders.get(&user(0)).map(|ids| ids.to_vec()),
            Some(vec![reused]),
            "one slot is left"
        );
        let b_seq = book
            .order_locations
            .get(&reused)
            .map(|location| location.user_seq)
            .expect("B is located");
        assert_eq!(
            book.user_orders
                .get(&user(0))
                .and_then(|ids| ids.id_at(b_seq)),
            Some(reused),
            "the slot left is B's, the one its location points at"
        );
        assert_eq!(
            book.get_order(reused)
                .map(|o| o.visible_quantity().as_u64()),
            Some(3)
        );
        assert_book_consistent(&book);
        // B's location still finds its own slot: cancelling B empties the
        // owner's entry.
        assert!(book.cancel_order(reused).expect("cancel").is_some());
        assert!(book.user_orders.get(&user(0)).is_none(), "entry dropped");
    }

    /// #311: an `UpdateQuantity` validated against a resting order, whose
    /// order a sweep then fully consumes before the level applies the
    /// update. The level reports the order absent and is left empty; the
    /// update returns `None` and leaves no location, user-index or risk
    /// entry behind.
    #[test]
    fn test_update_quantity_of_an_order_filled_mid_update_leaves_no_index() {
        let mut book = book();
        let maker = Id::from_u64(1);
        let (parked_tx, parked_rx) = channel::<()>();
        let (resume_tx, resume_rx) = channel::<()>();
        {
            let parked_tx = Mutex::new(parked_tx);
            let resume_rx = Mutex::new(resume_rx);
            let fired = AtomicBool::new(false);
            book.update_interleave_hook = Some(Arc::new(move |order_id: Id| {
                if order_id == maker && !fired.swap(true, Ordering::SeqCst) {
                    parked_tx
                        .lock()
                        .expect("parked sender")
                        .send(())
                        .expect("the test thread is waiting");
                    resume_rx
                        .lock()
                        .expect("resume receiver")
                        .recv_timeout(RENDEZVOUS_TIMEOUT)
                        .expect("resumed by the test thread");
                }
            }));
        }
        book.add_order(order(1, 0, Side::Sell, 100, 3))
            .expect("the maker rests");
        let book = Arc::new(book);
        let updater = {
            let book = Arc::clone(&book);
            thread::spawn(move || {
                book.update_order(pricelevel::OrderUpdate::UpdateQuantity {
                    order_id: maker,
                    new_quantity: Quantity::new(2),
                })
            })
        };
        parked_rx
            .recv_timeout(RENDEZVOUS_TIMEOUT)
            .expect("the update reached the hook");
        book.add_order(order(2, 1, Side::Buy, 100, 3))
            .expect("the taker fills the maker");
        resume_tx.send(()).expect("the update is parked");
        let updated = updater
            .join()
            .expect("updater")
            .expect("an absent order is not an error");
        assert!(updated.is_none(), "the order was gone");

        assert!(book.order_locations.get(&maker).is_none(), "no location");
        assert!(book.user_orders.get(&user(0)).is_none(), "no user entry");
        assert!(
            book.risk_state.orders.get(&maker).is_none(),
            "no risk entry"
        );
        assert_eq!(
            book.order_status(maker),
            Some(OrderStatus::Filled { filled_quantity: 3 })
        );
        assert!(book.asks.is_empty(), "the emptied level was removed");
        assert_book_consistent(&book);
    }

    /// 8 threads x 400 mixed operations per book, repeated over
    /// `SCENARIOS` books; the book must be consistent after each run.
    #[test]
    fn test_concurrent_crossing_adds_keep_indices_consistent() {
        for scenario in 0..SCENARIOS {
            let book = Arc::new(book());
            run_workers(&book, scenario + 1);
            assert_book_consistent(&book);
        }
    }

    /// The single-threaded run of the same workload is consistent too, so
    /// a failure of the concurrent test is an interleaving, not a
    /// bookkeeping bug of one of the operations.
    #[test]
    fn test_sequential_crossing_adds_keep_indices_consistent() {
        let book = book();
        let mut rng = Rng(0x2880_2880);
        for op in 0..(THREADS * OPS_PER_THREAD) {
            let raw = ID_STRIDE + op;
            let worker = op % THREADS;
            let side = if rng.below(2) == 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            let qty = rng.below(5) + 3;
            let price = match (side, rng.below(2)) {
                (Side::Buy, 0) => 99,
                (Side::Buy, _) => 101,
                (Side::Sell, 0) => 101,
                (Side::Sell, _) => 99,
            };
            let _ = book.add_order(order(raw, worker, side, price, qty));
            if op % 5 == 4 {
                let _ = book.cancel_order(Id::from_u64(ID_STRIDE + rng.below(op)));
            }
        }
        assert_book_consistent(&book);
    }
}
