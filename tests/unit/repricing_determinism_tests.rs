//! Integration tests for issue #106: repricing determinism and the
//! passive-side clamp that prevents a pegged re-price from crossing and
//! trading aggressively during a maintenance operation.
//!
//! These tests are gated on the `special_orders` feature, which is the only
//! configuration in which the repricing path exists.

#![cfg(feature = "special_orders")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use orderbook_rs::orderbook::repricing::RepricingOperations;
use orderbook_rs::{OrderBook, STPMode, StubClock, snapshots_match};
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

/// #174: a re-price whose `update_order` is rejected by the risk layer is no
/// longer silently swallowed — it is recorded in `RepricingResult.failed_orders`
/// with a reason, and (validate-first) the order keeps its prior price.
#[test]
fn reprice_records_risk_rejected_peg_in_failed_orders_issue_174() {
    use orderbook_rs::RiskConfig;

    let mut book: OrderBook<()> = OrderBook::new("TEST");
    book.set_risk_config(RiskConfig::new().with_max_notional_per_account(600));

    // Market maker (anonymous account) provides best bid 100, best ask 105.
    book.add_limit_order(Id::from_u64(1), 100, 1, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid admitted");
    book.add_limit_order(Id::from_u64(2), 105, 1, Side::Sell, TimeInForce::Gtc, None)
        .expect("ask admitted");

    // A pegged buy on its OWN account, resting passively at 90 (notional 540 <= 600).
    let pegger = Hash32::new([7u8; 32]);
    let peg_id = Id::from_u64(100);
    book.add_order(OrderType::PeggedOrder {
        id: peg_id,
        price: Price::new(90),
        quantity: Quantity::new(6),
        side: Side::Buy,
        user_id: pegger,
        timestamp: TimestampMs::new(1),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: 20,
        reference_price_type: PegReferenceType::BestBid,
        extra_fields: (),
    })
    .expect("peg admitted (540 <= 600)");

    // Re-price target = BestBid(100)+20 = 120, clamped to best_ask-1 = 104.
    // 6 * 104 = 624 > 600 -> the modify-aware risk check rejects the re-price,
    // which is now recorded (previously swallowed by `if ...is_ok()`).
    let result = book.reprice_special_orders().expect("reprice runs");
    assert_eq!(
        result.pegged_orders_repriced, 0,
        "the risk-rejected peg did not re-price"
    );
    assert_eq!(
        result.failed_orders.len(),
        1,
        "the rejection is recorded in failed_orders"
    );
    assert_eq!(result.failed_orders[0].0, peg_id);
    let reason = &result.failed_orders[0].1;
    assert!(
        reason.contains("rejected"),
        "the reason explains the failure: {reason}"
    );

    // Validate-first: the rejected re-price left the peg at its original price.
    let peg = book
        .get_order(peg_id)
        .expect("peg survives the rejected re-price");
    assert_eq!(
        peg.price().as_u128(),
        90,
        "a rejected re-price leaves the peg at its old price"
    );
}

// ---------------------------------------------------------------------------
// #328: each peg reads the touch as it stands when it is re-priced, without
// its own order, and a re-price never trades.
// ---------------------------------------------------------------------------

fn user(byte: u8) -> Hash32 {
    Hash32::new([byte; 32])
}

fn mid_peg(id: u64, price: u128, side: Side, owner: Hash32) -> OrderType<()> {
    OrderType::PeggedOrder {
        id: Id::from_u64(id),
        price: Price::new(price),
        quantity: Quantity::new(5),
        side,
        user_id: owner,
        timestamp: TimestampMs::new(1),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: 0,
        reference_price_type: PegReferenceType::MidPrice,
        extra_fields: (),
    }
}

/// The #328 book: a third user's bid 100 / ask 200, a buy mid peg (id 3)
/// resting at 100 and a sell mid peg (id 4) resting at 200, 5 lots each.
fn issue_328_book(
    stp: STPMode,
    buy_owner: Hash32,
    sell_owner: Hash32,
    trades: Arc<AtomicUsize>,
) -> OrderBook<()> {
    let counter = trades.clone();
    // A stub clock: two books built this way are identical, timestamps
    // included, so a sweep on each can be compared with `snapshots_match`.
    let mut book = OrderBook::<()>::with_clock("PEG/USD", Arc::new(StubClock::starting_at(1)));
    book.set_trade_listener(Arc::new(move |_trade| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    book.set_stp_mode(stp);
    let third = user(9);
    book.add_limit_order_with_user(
        Id::from_u64(1),
        100,
        5,
        Side::Buy,
        TimeInForce::Gtc,
        third,
        None,
    )
    .expect("third-party bid");
    book.add_limit_order_with_user(
        Id::from_u64(2),
        200,
        5,
        Side::Sell,
        TimeInForce::Gtc,
        third,
        None,
    )
    .expect("third-party ask");
    book.add_order(mid_peg(3, 100, Side::Buy, buy_owner))
        .expect("buy peg");
    book.add_order(mid_peg(4, 200, Side::Sell, sell_owner))
        .expect("sell peg");
    book
}

/// Runs one sweep on the #328 book and asserts the acceptance criteria.
fn assert_issue_328_sweep_is_passive(stp: STPMode, buy_owner: Hash32, sell_owner: Hash32) {
    let trades = Arc::new(AtomicUsize::new(0));
    let book = issue_328_book(stp, buy_owner, sell_owner, trades.clone());
    assert_eq!(trades.load(Ordering::SeqCst), 0, "setup must not trade");

    let result = book.reprice_special_orders().expect("sweep");

    assert_eq!(trades.load(Ordering::SeqCst), 0, "a sweep must never trade");
    assert!(
        result.failed_orders.is_empty(),
        "no re-price fails because of the other peg: {:?}",
        result.failed_orders
    );
    let buy = book
        .get_order(Id::from_u64(3))
        .expect("buy peg still rests");
    let sell = book
        .get_order(Id::from_u64(4))
        .expect("sell peg still rests");
    assert_eq!(buy.visible_quantity(), Quantity::new(5));
    assert_eq!(sell.visible_quantity(), Quantity::new(5));
    let bid = book.best_bid().expect("bid");
    let ask = book.best_ask().expect("ask");
    assert!(bid < ask, "book uncrossed: bid {bid} ask {ask}");
    // Buy peg reads mid(100, 200) = 150. Sell peg then reads bid 150 (the
    // buy peg) and ask 200 (itself excluded: the third-party ask shares the
    // level, so the level is the touch) = 175.
    assert_eq!(buy.price(), Price::new(150));
    assert_eq!(sell.price(), Price::new(175));
}

#[test]
fn test_reprice_opposite_mid_pegs_different_owners_no_trade() {
    assert_issue_328_sweep_is_passive(STPMode::None, user(1), user(2));
}

#[test]
fn test_reprice_opposite_mid_pegs_same_owner_stp_none_no_trade() {
    assert_issue_328_sweep_is_passive(STPMode::None, user(1), user(1));
}

#[test]
fn test_reprice_opposite_mid_pegs_same_owner_cancel_maker_no_cancel() {
    assert_issue_328_sweep_is_passive(STPMode::CancelMaker, user(1), user(1));
}

#[test]
fn test_reprice_opposite_mid_pegs_same_owner_cancel_taker_no_reject() {
    assert_issue_328_sweep_is_passive(STPMode::CancelTaker, user(1), user(1));
}

#[test]
fn test_reprice_opposite_mid_pegs_same_owner_cancel_both_no_reject() {
    assert_issue_328_sweep_is_passive(STPMode::CancelBoth, user(1), user(1));
}

#[test]
fn test_reprice_repeated_sweeps_unchanged_book_stable_prices() {
    let trades = Arc::new(AtomicUsize::new(0));
    let counter = trades.clone();
    let book = OrderBook::<()>::with_trade_listener(
        "PEG/USD",
        Arc::new(move |_trade| {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    book.add_limit_order(Id::from_u64(1), 100, 5, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid");
    book.add_limit_order(Id::from_u64(2), 200, 5, Side::Sell, TimeInForce::Gtc, None)
        .expect("ask");
    // Rests below the bid, so it is not the touch it reads.
    book.add_order(mid_peg(3, 90, Side::Buy, user(1)))
        .expect("peg");

    let mut prices = Vec::new();
    for _ in 0..3 {
        book.reprice_special_orders().expect("sweep");
        prices.push(book.get_order(Id::from_u64(3)).expect("peg").price());
    }
    // Old behaviour: 150, then 175 (mid of its own 150 bid and 200).
    assert_eq!(prices, vec![Price::new(150); 3]);
    assert_eq!(trades.load(Ordering::SeqCst), 0);
}

#[test]
fn test_reprice_peg_alone_at_best_reads_next_level() {
    let book = OrderBook::<()>::new("PEG/USD");
    book.add_limit_order(Id::from_u64(1), 100, 5, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid");
    book.add_limit_order(Id::from_u64(2), 200, 5, Side::Sell, TimeInForce::Gtc, None)
        .expect("ask");
    // Buy BestBid peg alone at 120 (the best bid): it reads 100, not itself.
    let peg = OrderType::PeggedOrder {
        id: Id::from_u64(3),
        price: Price::new(120),
        quantity: Quantity::new(5),
        side: Side::Buy,
        user_id: user(1),
        timestamp: TimestampMs::new(1),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: 1,
        reference_price_type: PegReferenceType::BestBid,
        extra_fields: (),
    };
    book.add_order(peg).expect("peg");
    book.reprice_special_orders().expect("sweep");
    assert_eq!(
        book.get_order(Id::from_u64(3)).expect("peg").price(),
        Price::new(101)
    );
    // Stable: a second sweep reads 100 again.
    book.reprice_special_orders().expect("sweep");
    assert_eq!(
        book.get_order(Id::from_u64(3)).expect("peg").price(),
        Price::new(101)
    );
}

#[test]
fn test_reprice_peg_sharing_best_level_reads_that_level() {
    let book = OrderBook::<()>::new("PEG/USD");
    book.add_limit_order(Id::from_u64(1), 120, 5, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid at the peg's level");
    book.add_limit_order(Id::from_u64(2), 100, 5, Side::Buy, TimeInForce::Gtc, None)
        .expect("deeper bid");
    book.add_limit_order(Id::from_u64(4), 200, 5, Side::Sell, TimeInForce::Gtc, None)
        .expect("ask");
    let peg = OrderType::PeggedOrder {
        id: Id::from_u64(3),
        price: Price::new(120),
        quantity: Quantity::new(5),
        side: Side::Buy,
        user_id: user(1),
        timestamp: TimestampMs::new(1),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: 1,
        reference_price_type: PegReferenceType::BestBid,
        extra_fields: (),
    };
    book.add_order(peg).expect("peg");
    book.reprice_special_orders().expect("sweep");
    // Another order rests at 120, so 120 is the touch: 120 + 1.
    assert_eq!(
        book.get_order(Id::from_u64(3)).expect("peg").price(),
        Price::new(121)
    );
}

#[test]
fn test_reprice_issue_328_sweep_identical_books_snapshots_match() {
    let a = issue_328_book(
        STPMode::CancelBoth,
        user(1),
        user(1),
        Arc::new(AtomicUsize::new(0)),
    );
    let b = issue_328_book(
        STPMode::CancelBoth,
        user(1),
        user(1),
        Arc::new(AtomicUsize::new(0)),
    );
    for _ in 0..3 {
        let ra = a.reprice_special_orders().expect("sweep a");
        let rb = b.reprice_special_orders().expect("sweep b");
        assert_eq!(ra.pegged_orders_repriced, rb.pegged_orders_repriced);
        assert_eq!(ra.failed_orders, rb.failed_orders);
    }
    let sa = a.create_snapshot(usize::MAX).expect("snapshot a");
    let sb = b.create_snapshot(usize::MAX).expect("snapshot b");
    assert!(snapshots_match(&sa, &sb));
}
