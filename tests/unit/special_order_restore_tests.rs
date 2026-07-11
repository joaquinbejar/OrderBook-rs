//! Restore-path re-registration of special orders with the re-pricing tracker
//! (Issue #194).
//!
//! `restore_from_snapshot` (the shared rebuild path behind both
//! `restore_from_snapshot_package` and the JSON entry points) rebuilds the
//! resting book but, before #194, left the `special_order_tracker`
//! freshly-initialized. A restored pegged / trailing-stop order was therefore
//! never re-registered and never re-priced again after a snapshot restore. The
//! fix re-registers every restored resting special order from the same
//! deterministic price-then-insertion-sequence rebuild pass that repopulates
//! `order_locations` / `user_orders`.
//!
//! The tracker holds only order ids; the trailing-stop watermark
//! (`last_reference_price`) and the pegged / stop price live in the order data
//! itself and survive the snapshot round-trip, so nothing is lost or
//! re-initialized — re-registering the id fully restores re-pricing.
//!
//! These tests are gated on `special_orders`, the only configuration in which
//! the tracker and the re-pricing path exist.

#![cfg(feature = "special_orders")]

use orderbook_rs::orderbook::repricing::RepricingOperations;
use orderbook_rs::{OrderBook, OrderBookSnapshot, snapshots_match};
use pricelevel::{
    Hash32, Id, OrderType, PegReferenceType, Price, Quantity, Side, TimeInForce, TimestampMs,
};

/// Build a two-sided book with best bid 100 / best ask 105 plus a passive Buy
/// pegged order resting at 90 that tracks best bid with a +20 offset. On a
/// re-price the peg would clamp to `best_ask - 1 = 104`.
fn book_with_passive_pegged(pegged_id: Id) -> OrderBook<()> {
    let book = OrderBook::<()>::new("PEG/USD");

    // Two-sided liquidity: best bid 100, best ask 105.
    let _ = book.add_limit_order(Id::from_u64(1), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(2), 99, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(3), 105, 10, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(4), 106, 10, Side::Sell, TimeInForce::Gtc, None);

    // Passive pegged buy resting at 90 (below the ask, does not cross).
    book.add_order(OrderType::PeggedOrder {
        id: pegged_id,
        price: Price::new(90),
        quantity: Quantity::new(5),
        side: Side::Buy,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(1),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: 20,
        reference_price_type: PegReferenceType::BestBid,
        extra_fields: (),
    })
    .expect("passive pegged order rests");

    book
}

/// #194: a pegged order restored from a snapshot package must be re-registered
/// with the tracker and actually re-price after restore.
///
/// Against the unfixed code the restored book's `pegged_order_count()` is 0,
/// `reprice_pegged_orders()` returns 0, and the order stays stuck at its
/// snapshotted price of 90 — so every assertion below fails.
#[test]
fn test_restore_reregisters_pegged_and_reprices_issue_194() {
    let pegged_id = Id::from_u64(1000);
    let book = book_with_passive_pegged(pegged_id);
    assert_eq!(book.pegged_order_count(), 1, "peg registered pre-snapshot");

    let json = match book.snapshot_to_json(usize::MAX) {
        Ok(j) => j,
        Err(e) => panic!("snapshot_to_json must succeed: {e}"),
    };

    let mut restored = OrderBook::<()>::new("PEG/USD");
    match restored.restore_from_snapshot_json(&json) {
        Ok(()) => {}
        Err(e) => panic!("restore must succeed: {e}"),
    }

    // The tracker is repopulated from the restored resting order (#194).
    assert_eq!(
        restored.pegged_order_count(),
        1,
        "restored book must re-register the pegged order with the tracker"
    );
    assert_eq!(restored.pegged_order_ids(), vec![pegged_id]);
    assert_eq!(
        restored.trailing_stop_count(),
        0,
        "no trailing stops present"
    );

    // The order rests at its snapshotted price before the re-price.
    let before = restored.get_order(pegged_id).expect("peg restored");
    assert_eq!(before.price().as_u128(), 90);

    // Firing the re-price now actually visits and moves the restored peg.
    let repriced = restored
        .reprice_pegged_orders()
        .expect("reprice runs on the restored book");
    assert_eq!(repriced, 1, "the restored peg must actually re-price");

    let after = restored.get_order(pegged_id).expect("peg still resting");
    let best_ask = restored.best_ask().expect("best ask present");
    assert_eq!(
        after.price().as_u128(),
        best_ask - 1,
        "restored peg clamps to best_ask - 1 = 104 after re-price"
    );
    assert!(after.price().as_u128() > 90, "the peg moved off 90");
}

/// #194: a trailing-stop order recovered from a snapshot must be re-registered
/// and re-price after restore, reading its watermark (`last_reference_price`)
/// straight from the restored order data.
///
/// A trailing stop only re-prices when its stop price sits inside the market
/// (a Sell stop below the bid), which the live matching path never lets rest —
/// it would trade on admission. A recovered snapshot legitimately holds one,
/// so we build the snapshot by merging a bid-only book with a stop-only book,
/// exactly the disaster-recovery shape #194 targets.
#[test]
fn test_restore_reregisters_trailing_stop_and_reprices_issue_194() {
    let stop_id = Id::from_u64(2000);

    // Market book: best bid 110 only.
    let market = OrderBook::<()>::new("TS/USD");
    let _ = market.add_limit_order(Id::from_u64(1), 110, 10, Side::Buy, TimeInForce::Gtc, None);
    let market_snapshot = market.create_snapshot(usize::MAX);

    // Stop book: a lone Sell trailing stop resting at 100 (empty book, so no
    // crossing on admission). Watermark 90, trail 5.
    let stop_book = OrderBook::<()>::new("TS/USD");
    stop_book
        .add_order(OrderType::TrailingStop {
            id: stop_id,
            price: Price::new(100),
            quantity: Quantity::new(5),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(1),
            time_in_force: TimeInForce::Gtc,
            trail_amount: Quantity::new(5),
            last_reference_price: Price::new(90),
            extra_fields: (),
        })
        .expect("lone trailing stop rests");
    let stop_snapshot = stop_book.create_snapshot(usize::MAX);

    // Merge: bid 110 from the market book, the Sell stop at 100 from the stop
    // book — a book that could not be built through live matching but is a valid
    // recovered snapshot.
    let merged = OrderBookSnapshot {
        symbol: "TS/USD".to_string(),
        timestamp: 0,
        bids: market_snapshot.bids,
        asks: stop_snapshot.asks,
    };

    let restored = OrderBook::<()>::new("TS/USD");
    match restored.restore_from_snapshot(merged) {
        Ok(()) => {}
        Err(e) => panic!("restore must succeed: {e}"),
    }

    // The tracker is repopulated from the restored resting stop (#194).
    assert_eq!(
        restored.trailing_stop_count(),
        1,
        "restored book must re-register the trailing stop with the tracker"
    );
    assert_eq!(restored.trailing_stop_ids(), vec![stop_id]);
    assert_eq!(restored.pegged_order_count(), 0, "no pegged orders present");
    assert_eq!(restored.best_bid(), Some(110), "bid liquidity restored");

    let before = restored.get_order(stop_id).expect("stop restored");
    assert_eq!(before.price().as_u128(), 100);

    // best_bid 110 > watermark 90, so the Sell stop trails up to
    // best_bid - trail = 105 and is re-priced. A trailing stop always trails
    // *toward* the market, so the re-priced 105 sits inside the bid (110) and
    // triggers — the re-price is validate-first modify + re-add, which matches
    // the now-marketable stop against the bid. The count returning 1 is the
    // #194 proof: the loop only visits (and re-prices) the stop because restore
    // re-registered it. Against the unfixed code the tracker is empty, so this
    // returns 0.
    let repriced = restored
        .reprice_trailing_stops()
        .expect("reprice runs on the restored book");
    assert_eq!(
        repriced, 1,
        "the restored trailing stop must actually re-price"
    );

    // The stop trailed into the bid and executed, so it no longer rests at its
    // stale price of 100 — the re-price genuinely acted on the restored order.
    assert!(
        restored.get_order(stop_id).is_none(),
        "the trailing stop trailed into the market and triggered on re-price"
    );
    assert_eq!(
        restored.best_bid(),
        Some(110),
        "the bid absorbed the triggered stop and is still the best bid"
    );
}

/// #194: a book with no special orders restores with an empty tracker, and a
/// mixed book re-registers only the special order — never the plain limits.
#[test]
fn test_restore_no_special_orders_leaves_tracker_empty_issue_194() {
    // Plain limit-only book.
    let plain = OrderBook::<()>::new("PLAIN/USD");
    let _ = plain.add_limit_order(Id::from_u64(1), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = plain.add_limit_order(Id::from_u64(2), 105, 10, Side::Sell, TimeInForce::Gtc, None);

    let json = match plain.snapshot_to_json(usize::MAX) {
        Ok(j) => j,
        Err(e) => panic!("snapshot_to_json must succeed: {e}"),
    };
    let mut restored_plain = OrderBook::<()>::new("PLAIN/USD");
    restored_plain
        .restore_from_snapshot_json(&json)
        .expect("restore plain book");

    assert_eq!(restored_plain.pegged_order_count(), 0);
    assert_eq!(restored_plain.trailing_stop_count(), 0);
    assert_eq!(restored_plain.best_bid(), Some(100));
    assert_eq!(restored_plain.best_ask(), Some(105));

    // Mixed book: a pegged order plus plain limits. Only the peg is tracked.
    let pegged_id = Id::from_u64(1000);
    let mixed = book_with_passive_pegged(pegged_id);
    let mixed_json = match mixed.snapshot_to_json(usize::MAX) {
        Ok(j) => j,
        Err(e) => panic!("snapshot_to_json must succeed: {e}"),
    };
    let mut restored_mixed = OrderBook::<()>::new("PEG/USD");
    restored_mixed
        .restore_from_snapshot_json(&mixed_json)
        .expect("restore mixed book");

    assert_eq!(restored_mixed.pegged_order_ids(), vec![pegged_id]);
    assert_eq!(restored_mixed.trailing_stop_count(), 0);
    // The plain limit orders are present but not tracked as special.
    assert!(restored_mixed.get_order(Id::from_u64(1)).is_some());
    assert!(restored_mixed.get_order(Id::from_u64(3)).is_some());
}

/// #194: the snapshot round-trip oracle still holds for a book that contains a
/// special order — re-registering the tracker must not perturb the resting book
/// structure the way `snapshots_match` compares it.
#[test]
fn test_restore_special_order_snapshot_round_trip_holds_issue_194() {
    let pegged_id = Id::from_u64(1000);
    let book = book_with_passive_pegged(pegged_id);
    let original = book.create_snapshot(usize::MAX);

    let mut restored = OrderBook::<()>::new("PEG/USD");
    let json = book.snapshot_to_json(usize::MAX).expect("snapshot json");
    restored
        .restore_from_snapshot_json(&json)
        .expect("restore succeeds");

    let round_trip = restored.create_snapshot(usize::MAX);
    assert!(
        snapshots_match(&round_trip, &original),
        "snapshot round-trip must hold for a book containing a special order"
    );

    // Re-pricing the restored peg and snapshotting again keeps the structure a
    // valid snapshot (the peg moved to 104, which the oracle sees as one bid
    // level relocating) — the tracker rebuild did not corrupt resting state.
    let repriced = restored.reprice_pegged_orders().expect("reprice runs");
    assert_eq!(repriced, 1);
    let after = restored.create_snapshot(usize::MAX);
    // The moved peg now rests at 104; the original snapshot had it at 90.
    assert!(
        !snapshots_match(&after, &original),
        "the re-price genuinely moved the peg, so the oracle now differs"
    );
    assert!(
        after.bids.iter().any(|l| l.price().as_u128() == 104),
        "the re-priced peg rests at 104 in the post-reprice snapshot"
    );
}
