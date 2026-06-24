//! Integration tests for issue #106: repricing determinism and the
//! passive-side clamp that prevents a pegged re-price from crossing and
//! trading aggressively during a maintenance operation.
//!
//! These tests are gated on the `special_orders` feature, which is the only
//! configuration in which the repricing path exists.

#![cfg(feature = "special_orders")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use orderbook_rs::OrderBook;
use orderbook_rs::orderbook::repricing::RepricingOperations;
use pricelevel::{
    Hash32, Id, OrderType, PegReferenceType, Price, Quantity, Side, TimeInForce, TimestampMs,
};

/// Builds a two-sided book with a clear spread: best bid 100, best ask 105.
fn two_sided_book(trade_count: Arc<AtomicUsize>) -> OrderBook<()> {
    let counter = trade_count.clone();
    let book = OrderBook::<()>::with_trade_listener(
        "TEST/USD",
        Arc::new(move |_trade| {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );

    // Bids below the spread.
    let _ = book.add_limit_order(Id::from_u64(1), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(2), 99, 10, Side::Buy, TimeInForce::Gtc, None);
    // Asks above the spread.
    let _ = book.add_limit_order(Id::from_u64(3), 105, 10, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(4), 106, 10, Side::Sell, TimeInForce::Gtc, None);

    book
}

#[test]
fn buy_pegged_reprice_does_not_cross_or_trade_issue_106() {
    let trade_count = Arc::new(AtomicUsize::new(0));
    let book = two_sided_book(trade_count.clone());

    assert_eq!(book.best_bid(), Some(100));
    assert_eq!(book.best_ask(), Some(105));

    // Buy peg tracking best bid with a +20 offset would land at 120, well above
    // the best ask (105). Without the clamp this would cross and fill.
    let pegged_id = Id::from_u64(1000);
    let pegged = OrderType::PeggedOrder {
        id: pegged_id,
        price: Price::new(90), // resting, passive initial price
        quantity: Quantity::new(5),
        side: Side::Buy,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(1),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: 20,
        reference_price_type: PegReferenceType::BestBid,
        extra_fields: (),
    };
    book.add_order(pegged)
        .expect("pegged order added passively");

    // Adding the resting peg must not have traded.
    assert_eq!(
        trade_count.load(Ordering::SeqCst),
        0,
        "passive pegged add must not trade"
    );

    let repriced = book.reprice_pegged_orders().expect("reprice succeeds");
    assert_eq!(repriced, 1, "the pegged order should have been re-priced");

    // (i) No trade was emitted by the reprice.
    assert_eq!(
        trade_count.load(Ordering::SeqCst),
        0,
        "reprice must not aggressively trade"
    );

    // (ii) The order rests passively strictly below the best ask.
    let order = book
        .get_order(pegged_id)
        .expect("pegged order still resting");
    let best_ask = book.best_ask().expect("best ask present");
    assert!(
        order.price().as_u128() < best_ask,
        "buy peg must rest below best ask: price={} best_ask={}",
        order.price(),
        best_ask
    );
    // Clamp lands exactly at best_ask - 1.
    assert_eq!(order.price().as_u128(), best_ask - 1);
}

#[test]
fn sell_pegged_reprice_does_not_cross_or_trade_issue_106() {
    let trade_count = Arc::new(AtomicUsize::new(0));
    let book = two_sided_book(trade_count.clone());

    assert_eq!(book.best_bid(), Some(100));
    assert_eq!(book.best_ask(), Some(105));

    // Sell peg tracking best ask with a -20 offset would land at 85, below the
    // best bid (100). Without the clamp this would cross and fill.
    let pegged_id = Id::from_u64(2000);
    let pegged = OrderType::PeggedOrder {
        id: pegged_id,
        price: Price::new(115), // resting, passive initial price
        quantity: Quantity::new(5),
        side: Side::Sell,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(1),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: -20,
        reference_price_type: PegReferenceType::BestAsk,
        extra_fields: (),
    };
    book.add_order(pegged)
        .expect("pegged order added passively");

    assert_eq!(
        trade_count.load(Ordering::SeqCst),
        0,
        "passive pegged add must not trade"
    );

    let repriced = book.reprice_pegged_orders().expect("reprice succeeds");
    assert_eq!(repriced, 1, "the pegged order should have been re-priced");

    // (i) No trade was emitted by the reprice.
    assert_eq!(
        trade_count.load(Ordering::SeqCst),
        0,
        "reprice must not aggressively trade"
    );

    // (ii) The order rests passively strictly above the best bid.
    let order = book
        .get_order(pegged_id)
        .expect("pegged order still resting");
    let best_bid = book.best_bid().expect("best bid present");
    assert!(
        order.price().as_u128() > best_bid,
        "sell peg must rest above best bid: price={} best_bid={}",
        order.price(),
        best_bid
    );
    // Clamp lands exactly at best_bid + 1.
    assert_eq!(order.price().as_u128(), best_bid + 1);
}

/// Re-pricing visits pegged orders in the deterministic `to_string`-sorted
/// order regardless of registration order. We register several pegged orders
/// whose insertion order differs from the sorted order and assert the book's
/// `pegged_order_ids()` view (the same source the reprice loop consumes) is
/// stable and sorted.
#[test]
fn reprice_visits_pegged_ids_in_deterministic_order_issue_106() {
    let book = OrderBook::<()>::new("TEST/USD");

    // Liquidity so reference prices exist.
    let _ = book.add_limit_order(Id::from_u64(1), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(2), 105, 10, Side::Sell, TimeInForce::Gtc, None);

    // Register pegged orders with ids whose insertion order != sorted order.
    let ids = [
        Id::sequential(10),
        Id::sequential(2),
        Id::sequential(33),
        Id::sequential(1),
    ];
    for (i, id) in ids.iter().enumerate() {
        let pegged = OrderType::PeggedOrder {
            id: *id,
            price: Price::new(80 + i as u128),
            quantity: Quantity::new(1),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(1),
            time_in_force: TimeInForce::Gtc,
            reference_price_offset: -5,
            reference_price_type: PegReferenceType::BestBid,
            extra_fields: (),
        };
        book.add_order(pegged).expect("pegged order added");
    }

    let mut expected = ids.to_vec();
    expected.sort_by_key(|id| id.to_string());

    let got = book.pegged_order_ids();
    assert_eq!(got, expected, "pegged ids must be to_string-sorted");
    // Stable across repeated reads.
    assert_eq!(book.pegged_order_ids(), got);
}

/// BLOCKER regression for issue #106: on a `tick_size > 1` book, a clamped peg
/// price must be tick-aligned so the re-insert actually succeeds.
///
/// The old `±1` clamp produced `best_ask - 1`, which is off-tick on a tick=5
/// book. `add_order` rejects off-tick prices with `InvalidTickSize`, and the
/// re-price path swallows that error (`if self.update_order(..).is_ok()`), so
/// the order would be silently left at its stale price — defeating #106. With
/// the tick-aware clamp the order moves to `best_ask - tick` (tick-aligned,
/// strictly below the ask) and never trades.
///
/// This test FAILS against the old `±1` code (the order stays at 90) and passes
/// with the tick-aware snap (the order moves to 100).
#[test]
fn buy_pegged_reprice_tick_aligned_and_moves_issue_106() {
    let trade_count = Arc::new(AtomicUsize::new(0));
    let counter = trade_count.clone();

    let mut book = OrderBook::<()>::with_tick_size("TICK/USD", 5);
    book.set_trade_listener(Arc::new(move |_trade| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    // Tick-aligned (multiples of 5) two-sided liquidity: best bid 100, ask 105.
    let _ = book.add_limit_order(Id::from_u64(1), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(2), 95, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(3), 105, 10, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(4), 110, 10, Side::Sell, TimeInForce::Gtc, None);

    assert_eq!(book.best_bid(), Some(100));
    assert_eq!(book.best_ask(), Some(105));

    // Buy peg tracking best bid + 7 = 107 -> would cross ask 105. Initial resting
    // price 90 (tick-aligned, passive).
    let pegged_id = Id::from_u64(1000);
    let pegged = OrderType::PeggedOrder {
        id: pegged_id,
        price: Price::new(90),
        quantity: Quantity::new(5),
        side: Side::Buy,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(1),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: 7,
        reference_price_type: PegReferenceType::BestBid,
        extra_fields: (),
    };
    book.add_order(pegged)
        .expect("pegged order added passively");
    assert_eq!(
        trade_count.load(Ordering::SeqCst),
        0,
        "passive add must not trade"
    );

    let repriced = book.reprice_pegged_orders().expect("reprice succeeds");
    assert_eq!(repriced, 1, "the pegged order must actually re-price");

    // No trade emitted by the reprice.
    assert_eq!(
        trade_count.load(Ordering::SeqCst),
        0,
        "reprice must not aggressively trade"
    );

    // The order actually MOVED to a tick-aligned passive price: best_ask - tick.
    let order = book
        .get_order(pegged_id)
        .expect("pegged order still resting");
    let new_price = order.price().as_u128();
    assert_eq!(
        new_price, 100,
        "must snap to best_ask - tick (tick-aligned), not stay at stale 90"
    );
    assert!(new_price.is_multiple_of(5), "must be tick-aligned");
    let best_ask = book.best_ask().expect("best ask present");
    assert!(new_price < best_ask, "must rest below best ask");
}

/// Symmetric Sell case on a `tick_size = 5` book: a sell peg whose raw price
/// lands below the bid must snap UP to a tick-aligned price strictly above the
/// bid (`best_bid + tick`) and must not trade.
#[test]
fn sell_pegged_reprice_tick_aligned_and_moves_issue_106() {
    let trade_count = Arc::new(AtomicUsize::new(0));
    let counter = trade_count.clone();

    let mut book = OrderBook::<()>::with_tick_size("TICK/USD", 5);
    book.set_trade_listener(Arc::new(move |_trade| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    let _ = book.add_limit_order(Id::from_u64(1), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(2), 95, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(3), 120, 10, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::from_u64(4), 125, 10, Side::Sell, TimeInForce::Gtc, None);

    assert_eq!(book.best_bid(), Some(100));
    assert_eq!(book.best_ask(), Some(120));

    // Sell peg tracking best ask (120) - 35 = 85 -> below bid 100. Initial
    // resting price 130 (tick-aligned, passive).
    let pegged_id = Id::from_u64(2000);
    let pegged = OrderType::PeggedOrder {
        id: pegged_id,
        price: Price::new(130),
        quantity: Quantity::new(5),
        side: Side::Sell,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(1),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: -35,
        reference_price_type: PegReferenceType::BestAsk,
        extra_fields: (),
    };
    book.add_order(pegged)
        .expect("pegged order added passively");
    assert_eq!(
        trade_count.load(Ordering::SeqCst),
        0,
        "passive add must not trade"
    );

    let repriced = book.reprice_pegged_orders().expect("reprice succeeds");
    assert_eq!(repriced, 1, "the pegged order must actually re-price");
    assert_eq!(
        trade_count.load(Ordering::SeqCst),
        0,
        "reprice must not aggressively trade"
    );

    let order = book
        .get_order(pegged_id)
        .expect("pegged order still resting");
    let new_price = order.price().as_u128();
    assert_eq!(
        new_price, 105,
        "must snap to best_bid + tick (tick-aligned), not stay at stale 130"
    );
    assert!(new_price.is_multiple_of(5), "must be tick-aligned");
    let best_bid = book.best_bid().expect("best bid present");
    assert!(new_price > best_bid, "must rest above best bid");
}
