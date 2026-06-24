//! Atomic-modify regression tests (#98).
//!
//! `UpdatePrice` / `UpdatePriceAndQuantity` / `Replace` are validate-first:
//! the new order is validated (shape checks + modify-aware risk check)
//! *before* the original is cancelled. A rejected modify must therefore
//! leave the original resting order completely untouched — no book
//! mutation, no events, no trades — and surface the typed error.

use orderbook_rs::orderbook::trade::{TradeListener, TradeResult};
use orderbook_rs::{OrderBook, OrderBookError, ReferencePriceSource, RiskConfig};
use pricelevel::{Hash32, Id, OrderUpdate, Price, Quantity, Side, TimeInForce};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

fn account(byte: u8) -> Hash32 {
    Hash32::new([byte; 32])
}

/// Build a book that counts every emitted trade through a listener so we can
/// assert that a rejected modify emits no trade at all.
fn book_with_trade_counter() -> (OrderBook<()>, Arc<AtomicU64>) {
    let count = Arc::new(AtomicU64::new(0));
    let listener_count = Arc::clone(&count);
    let listener: TradeListener = Arc::new(move |_: &TradeResult| {
        listener_count.fetch_add(1, Ordering::Relaxed);
    });
    let book = OrderBook::<()>::with_trade_listener("TEST", listener);
    (book, count)
}

// ───────────────────────────────────────────────────────────────────────
// Post-only modify that would cross the spread
// ───────────────────────────────────────────────────────────────────────

#[test]
fn post_only_modify_that_would_cross_leaves_original_untouched_and_emits_no_trade() {
    let (book, trade_count) = book_with_trade_counter();

    // Resting ask at 110 to be crossed by an aggressive buy.
    book.add_limit_order(Id::new_uuid(), 110, 10, Side::Sell, TimeInForce::Gtc, None)
        .expect("resting ask admitted");

    // Resting post-only buy at 100 (does not cross the 110 ask).
    let buy_id = Id::new_uuid();
    book.add_post_only_order(buy_id, 100, 5, Side::Buy, TimeInForce::Gtc, None)
        .expect("post-only buy admitted");

    // No trades yet.
    assert_eq!(trade_count.load(Ordering::Relaxed), 0);

    // Modify the post-only buy up to 115 — this would cross the 110 ask, so a
    // post-only must be rejected with PriceCrossing.
    let result = book.update_order(OrderUpdate::UpdatePrice {
        order_id: buy_id,
        new_price: Price::new(115),
    });
    match result {
        Err(OrderBookError::PriceCrossing { price, side, .. }) => {
            assert_eq!(price, 115);
            assert_eq!(side, Side::Buy);
        }
        other => panic!("expected PriceCrossing, got {other:?}"),
    }

    // The original order must survive completely unchanged.
    let survivor = book
        .get_order(buy_id)
        .expect("original order still resting");
    assert_eq!(survivor.id(), buy_id);
    assert_eq!(survivor.price().as_u128(), 100, "price unchanged");
    assert_eq!(
        survivor.visible_quantity(),
        Quantity::new(5),
        "quantity unchanged"
    );
    assert_eq!(survivor.side(), Side::Buy);

    // Still tracked at the original price/side.
    assert_eq!(book.best_bid(), Some(100));

    // And crucially: no trade was emitted by the rejected modify.
    assert_eq!(
        trade_count.load(Ordering::Relaxed),
        0,
        "a rejected modify must not emit any trade"
    );
}

#[test]
fn post_only_replace_that_would_cross_leaves_original_untouched() {
    let book: OrderBook<()> = OrderBook::new("TEST");

    book.add_limit_order(Id::new_uuid(), 110, 10, Side::Sell, TimeInForce::Gtc, None)
        .expect("resting ask admitted");

    let buy_id = Id::new_uuid();
    book.add_post_only_order(buy_id, 100, 5, Side::Buy, TimeInForce::Gtc, None)
        .expect("post-only buy admitted");

    // Replace re-prices the post-only buy up to 120 (crosses the 110 ask).
    let result = book.update_order(OrderUpdate::Replace {
        order_id: buy_id,
        price: Price::new(120),
        quantity: Quantity::new(7),
        side: Side::Buy,
    });
    assert!(
        matches!(result, Err(OrderBookError::PriceCrossing { .. })),
        "expected PriceCrossing, got {result:?}"
    );

    let survivor = book
        .get_order(buy_id)
        .expect("original survives the rejected replace");
    assert_eq!(survivor.price().as_u128(), 100);
    assert_eq!(survivor.visible_quantity(), Quantity::new(5));
}

// ───────────────────────────────────────────────────────────────────────
// Risk-limit-breaching modify
// ───────────────────────────────────────────────────────────────────────

#[test]
fn modify_outside_price_band_leaves_original_untouched() {
    let mut book: OrderBook<()> = OrderBook::new("TEST");
    // 100 bps = 1% band around the mid.
    book.set_risk_config(RiskConfig::new().with_price_band_bps(100, ReferencePriceSource::Mid));
    let acct = account(31);

    // Two-sided book establishes a mid of 100.
    book.add_limit_order_with_user(
        Id::new_uuid(),
        90,
        10,
        Side::Buy,
        TimeInForce::Gtc,
        acct,
        None,
    )
    .expect("bid admitted");
    book.add_limit_order_with_user(
        Id::new_uuid(),
        110,
        10,
        Side::Sell,
        TimeInForce::Gtc,
        acct,
        None,
    )
    .expect("ask admitted");

    // Resting order we will try to modify.
    let order_id = Id::new_uuid();
    book.add_limit_order_with_user(order_id, 99, 5, Side::Buy, TimeInForce::Gtc, acct, None)
        .expect("order admitted within band");

    // Modify to 200 — far outside the 1% band around mid 100. Rejected.
    let result = book.update_order(OrderUpdate::UpdatePrice {
        order_id,
        new_price: Price::new(200),
    });
    assert!(
        matches!(result, Err(OrderBookError::RiskPriceBand { .. })),
        "expected RiskPriceBand, got {result:?}"
    );

    // Original survives unchanged.
    let survivor = book.get_order(order_id).expect("original survives");
    assert_eq!(survivor.price().as_u128(), 99);
    assert_eq!(survivor.visible_quantity(), Quantity::new(5));
}

#[test]
fn modify_over_max_notional_leaves_original_untouched() {
    let mut book: OrderBook<()> = OrderBook::new("TEST");
    // Notional ceiling 1_000 per account.
    book.set_risk_config(RiskConfig::new().with_max_notional_per_account(1_000));
    let acct = account(32);

    // Resting order contributes 100 * 5 = 500 of notional.
    let order_id = Id::new_uuid();
    book.add_limit_order_with_user(order_id, 100, 5, Side::Buy, TimeInForce::Gtc, acct, None)
        .expect("order admitted within notional");

    // Modify to 100 * 20 = 2_000 → projected 500 - 500 + 2_000 = 2_000 > 1_000.
    let result = book.update_order(OrderUpdate::UpdatePriceAndQuantity {
        order_id,
        new_price: Price::new(100),
        new_quantity: Quantity::new(20),
    });
    match result {
        Err(OrderBookError::RiskMaxNotional {
            account: a,
            attempted,
            limit,
            ..
        }) => {
            assert_eq!(a, acct);
            assert_eq!(attempted, 2_000);
            assert_eq!(limit, 1_000);
        }
        other => panic!("expected RiskMaxNotional, got {other:?}"),
    }

    // Original survives unchanged.
    let survivor = book.get_order(order_id).expect("original survives");
    assert_eq!(survivor.price().as_u128(), 100);
    assert_eq!(survivor.visible_quantity(), Quantity::new(5));
}

// ───────────────────────────────────────────────────────────────────────
// Valid modify at exactly max_open_orders_per_account must succeed
// ───────────────────────────────────────────────────────────────────────

#[test]
fn modify_at_max_open_orders_succeeds() {
    let mut book: OrderBook<()> = OrderBook::new("TEST");
    // Account may hold at most 2 resting orders.
    book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(2));
    let acct = account(33);

    // Fill the open-order quota: account is now exactly at the limit.
    let target_id = Id::new_uuid();
    book.add_limit_order_with_user(target_id, 100, 3, Side::Buy, TimeInForce::Gtc, acct, None)
        .expect("first order admitted");
    book.add_limit_order_with_user(
        Id::new_uuid(),
        101,
        3,
        Side::Buy,
        TimeInForce::Gtc,
        acct,
        None,
    )
    .expect("second order admitted (now at limit)");

    // A modify keeps the count unchanged (one out, one in) so it must SUCCEED
    // even though the account is at max_open_orders. This is the regression
    // guard for the false rejection the modify-aware risk check prevents: a
    // naive re-use of the limit-admission check would reject here.
    let result = book.update_order(OrderUpdate::UpdatePrice {
        order_id: target_id,
        new_price: Price::new(95),
    });
    assert!(
        result.is_ok(),
        "modify at max_open_orders must succeed, got {result:?}"
    );

    let modified = book.get_order(target_id).expect("modified order resting");
    assert_eq!(modified.price().as_u128(), 95, "price was updated");
    assert_eq!(modified.visible_quantity(), Quantity::new(3));
}
