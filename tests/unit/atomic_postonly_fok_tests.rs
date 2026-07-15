//! #209: PostOnly and multi-level FOK decisions are atomic at the book
//! boundary.
//!
//! - PostOnly threads `TakerKind::PostOnly` into every per-level match, so
//!   pricelevel's structural guard makes trading impossible under any
//!   interleaving; the `will_cross_market` precheck is only a fast path.
//! - A fill-or-kill submit holds the submit gate exclusively across its
//!   feasibility check and sweep, so concurrent adds/cancels cannot turn
//!   all-or-nothing into a partial execution.
//!
//! The race regressions are barrier-synchronized threaded loops: the
//! asserted invariants are structural, so they must hold under every
//! interleaving the loops produce.

#[cfg(test)]
mod tests_atomic_postonly_fok {
    use orderbook_rs::{DefaultOrderBook, OrderBook, OrderBookError, TradeResult};
    use pricelevel::{Hash32, Id, Side, TimeInForce};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    /// Deterministic: a post-only submit into a crossed book is rejected
    /// with `PriceCrossing` and zero trades.
    #[test]
    fn post_only_into_crossed_book_rejects_with_zero_trades() {
        let book: OrderBook<()> = DefaultOrderBook::new("POX");
        book.add_limit_order(Id::from_u64(1), 100, 10, Side::Sell, TimeInForce::Gtc, None)
            .expect("seed ask");

        let err = book
            .add_post_only_order(Id::from_u64(2), 100, 5, Side::Buy, TimeInForce::Gtc, None)
            .expect_err("crossing post-only must be rejected");
        assert!(
            matches!(err, OrderBookError::PriceCrossing { .. }),
            "expected PriceCrossing, got {err:?}"
        );
        assert!(book.last_trade_price().is_none(), "zero trades emitted");
        assert!(
            book.get_order(Id::from_u64(2)).is_none(),
            "rejected post-only never rests"
        );
    }

    /// Deterministic: multi-level FOK stays all-or-nothing single-threaded
    /// (full fill across two levels; kill with zero trades when short).
    #[test]
    fn multi_level_fok_all_or_nothing_deterministic() {
        let book: OrderBook<()> = DefaultOrderBook::new("FOKD");
        book.add_limit_order(Id::from_u64(1), 100, 5, Side::Sell, TimeInForce::Gtc, None)
            .expect("seed ask 100");
        book.add_limit_order(Id::from_u64(2), 101, 5, Side::Sell, TimeInForce::Gtc, None)
            .expect("seed ask 101");

        // Short by 5 units: killed with zero fills.
        let err = book
            .add_limit_order(Id::from_u64(3), 101, 15, Side::Buy, TimeInForce::Fok, None)
            .expect_err("infeasible FOK must be killed");
        assert!(
            matches!(err, OrderBookError::InsufficientLiquidity { .. }),
            "expected InsufficientLiquidity, got {err:?}"
        );
        assert!(book.last_trade_price().is_none(), "kill emits zero trades");

        // Exactly feasible: fills both levels completely.
        let (_, trade_result) = book
            .add_order_with_result(pricelevel::OrderType::Standard {
                id: Id::from_u64(4),
                price: pricelevel::Price::new(101),
                quantity: pricelevel::Quantity::new(10),
                side: Side::Buy,
                user_id: Hash32::zero(),
                timestamp: pricelevel::TimestampMs::new(0),
                time_in_force: TimeInForce::Fok,
                extra_fields: (),
            })
            .expect("feasible FOK fills in full");
        let filled: u64 = trade_result
            .as_ref()
            .map(fills_in(Id::from_u64(4)))
            .unwrap_or(0);
        assert_eq!(filled, 10, "FOK filled its complete quantity");
    }

    /// Sums the taker's filled quantity in a `TradeResult`.
    fn fills_in(taker: Id) -> impl Fn(&TradeResult) -> u64 {
        move |tr: &TradeResult| {
            tr.match_result
                .trades()
                .as_vec()
                .iter()
                .filter(|t| t.taker_order_id() == taker)
                .map(|t| t.quantity().as_u64())
                .sum()
        }
    }

    /// Race regression (issue repro): a contra maker is admitted while a
    /// post-only submit is in flight. Under the structural guard the
    /// post-only order must never appear as taker in any trade, in any
    /// interleaving.
    #[test]
    fn post_only_never_trades_under_concurrent_contra_admission() {
        const ROUNDS: u64 = 200;
        let violations = Arc::new(AtomicU64::new(0));

        for round in 0..ROUNDS {
            let mut book: OrderBook<()> = DefaultOrderBook::new("PORACE");
            let trades: Arc<Mutex<Vec<(Id, u64)>>> = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&trades);
            book.trade_listener = Some(Arc::new(move |tr: &TradeResult| {
                let mut sunk = sink.lock().expect("trade sink lock");
                for t in tr.match_result.trades().as_vec() {
                    sunk.push((t.taker_order_id(), t.quantity().as_u64()));
                }
            }));
            let book = Arc::new(book);

            let po_id = Id::from_u64(1_000 + round);
            let barrier = Arc::new(Barrier::new(2));

            let contra_book = Arc::clone(&book);
            let contra_barrier = Arc::clone(&barrier);
            let contra = thread::spawn(move || {
                contra_barrier.wait();
                // Crossing ask admitted concurrently with the post-only buy.
                let _ = contra_book.add_limit_order(
                    Id::from_u64(500_000 + round),
                    100,
                    5,
                    Side::Sell,
                    TimeInForce::Gtc,
                    None,
                );
            });

            let po_book = Arc::clone(&book);
            let po_barrier = Arc::clone(&barrier);
            let po = thread::spawn(move || {
                po_barrier.wait();
                // Either rests (no cross yet) or is rejected (cross seen) —
                // both fine; trading is the only forbidden outcome.
                let _ =
                    po_book.add_post_only_order(po_id, 100, 5, Side::Buy, TimeInForce::Gtc, None);
            });

            contra.join().expect("contra thread");
            po.join().expect("post-only thread");

            let taker_fills: u64 = trades
                .lock()
                .expect("trade sink lock")
                .iter()
                .filter(|(taker, _)| *taker == po_id)
                .map(|(_, qty)| *qty)
                .sum();
            if taker_fills > 0 {
                violations.fetch_add(1, Ordering::Relaxed);
            }
        }

        assert_eq!(
            violations.load(Ordering::Relaxed),
            0,
            "a post-only order took liquidity in some interleaving"
        );
    }

    /// Race regression (issue repro): a later-level maker is cancelled
    /// while a multi-level FOK is in flight. Under the exclusive submit
    /// gate every FOK outcome is all-or-nothing: its fills are exactly 0
    /// or exactly the full quantity — never partial.
    #[test]
    fn multi_level_fok_never_partial_under_concurrent_cancel() {
        const ROUNDS: u64 = 200;
        const FOK_QTY: u64 = 10;

        for round in 0..ROUNDS {
            let book: Arc<OrderBook<()>> = Arc::new(DefaultOrderBook::new("FOKRACE"));
            book.add_limit_order(Id::from_u64(1), 100, 5, Side::Sell, TimeInForce::Gtc, None)
                .expect("seed ask 100");
            book.add_limit_order(Id::from_u64(2), 101, 5, Side::Sell, TimeInForce::Gtc, None)
                .expect("seed ask 101");

            let fok_id = Id::from_u64(10_000 + round);
            let barrier = Arc::new(Barrier::new(2));

            let cancel_book = Arc::clone(&book);
            let cancel_barrier = Arc::clone(&barrier);
            let canceller = thread::spawn(move || {
                cancel_barrier.wait();
                let _ = cancel_book.cancel_order(Id::from_u64(2));
            });

            let fok_book = Arc::clone(&book);
            let fok_barrier = Arc::clone(&barrier);
            let fok = thread::spawn(move || {
                fok_barrier.wait();
                fok_book.add_order_with_result(pricelevel::OrderType::Standard {
                    id: fok_id,
                    price: pricelevel::Price::new(101),
                    quantity: pricelevel::Quantity::new(FOK_QTY),
                    side: Side::Buy,
                    user_id: Hash32::zero(),
                    timestamp: pricelevel::TimestampMs::new(0),
                    time_in_force: TimeInForce::Fok,
                    extra_fields: (),
                })
            });

            canceller.join().expect("cancel thread");
            let outcome = fok.join().expect("fok thread");

            let filled = match &outcome {
                Ok((_, trade_result)) => trade_result.as_ref().map(fills_in(fok_id)).unwrap_or(0),
                Err(_) => 0,
            };
            assert!(
                filled == 0 || filled == FOK_QTY,
                "round {round}: FOK filled {filled} of {FOK_QTY} — partial execution leaked"
            );
            // An Ok FOK must be a FULL fill, never a resting remainder.
            if let Ok((_, trade_result)) = &outcome {
                let filled_ok = trade_result.as_ref().map(fills_in(fok_id)).unwrap_or(0);
                assert_eq!(
                    filled_ok, FOK_QTY,
                    "round {round}: Ok FOK must fill in full"
                );
                assert!(
                    book.get_order(fok_id).is_none(),
                    "round {round}: FOK never rests"
                );
            }
        }
    }
}

/// #209 review follow-up: post-only precedence over STP. The crossability
/// verdict is resolved BEFORE the STP block, so a rejected post-only is a
/// pure no-op even when the crossing liquidity belongs to the same user
/// under CancelMaker — no maker is cancelled as a side effect, and no
/// trade is ever emitted.
#[cfg(test)]
mod tests_post_only_stp_precedence {
    use orderbook_rs::{DefaultOrderBook, OrderBook, STPMode, TradeResult};
    use pricelevel::{Hash32, Id, Side, TimeInForce};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    #[test]
    fn post_only_rejection_never_cancels_same_user_makers() {
        const ROUNDS: u64 = 200;
        let user = Hash32::new([7u8; 32]);

        for round in 0..ROUNDS {
            let mut book: OrderBook<()> = DefaultOrderBook::new("POSTP");
            book.set_stp_mode(STPMode::CancelMaker);
            let trades: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
            let sink = Arc::clone(&trades);
            book.trade_listener = Some(Arc::new(move |tr: &TradeResult| {
                *sink.lock().expect("sink") += tr.match_result.trades().len() as u64;
            }));
            let book = Arc::new(book);

            let ask_id = Id::from_u64(500_000 + round);
            let po_id = Id::from_u64(1_000 + round);
            let barrier = Arc::new(Barrier::new(2));

            let ask_book = Arc::clone(&book);
            let ask_barrier = Arc::clone(&barrier);
            let ask_thread = thread::spawn(move || {
                ask_barrier.wait();
                ask_book.add_limit_order_with_user(
                    ask_id,
                    100,
                    5,
                    Side::Sell,
                    TimeInForce::Gtc,
                    user,
                    None,
                )
            });

            let po_book = Arc::clone(&book);
            let po_barrier = Arc::clone(&barrier);
            let po_thread = thread::spawn(move || {
                po_barrier.wait();
                po_book.add_post_only_order_with_user(
                    po_id,
                    100,
                    5,
                    Side::Buy,
                    TimeInForce::Gtc,
                    user,
                    None,
                )
            });

            let ask_outcome = ask_thread.join().expect("ask thread");
            let po_outcome = po_thread.join().expect("post-only thread");

            // Same-user post-only vs same-user ask: no interleaving may
            // produce a trade (post-only never takes; the ask taker is
            // self-trade-prevented against a resting post-only).
            assert_eq!(
                *trades.lock().expect("sink"),
                0,
                "round {round}: zero trades under every interleaving"
            );

            // Post-only precedence over STP: a rejected post-only is a
            // pure no-op — the same-user ask (when it rested) keeps its
            // full quantity; CancelMaker must not have fired for it.
            if po_outcome.is_err() && ask_outcome.is_ok() {
                let ask = book
                    .get_order(ask_id)
                    .expect("round {round}: rejected post-only must leave the ask resting");
                assert_eq!(
                    ask.visible_quantity().as_u64(),
                    5,
                    "round {round}: rejected post-only must not touch the same-user maker"
                );
            }
        }
    }
}
