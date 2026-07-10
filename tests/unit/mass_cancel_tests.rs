//! Integration tests for mass cancel operations.

use orderbook_rs::orderbook::mass_cancel::MassCancelResult;
use orderbook_rs::{OrderBook, STPMode};
use pricelevel::{Hash32, Id, Side, TimeInForce};

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn new_book() -> OrderBook<()> {
    OrderBook::new("TEST")
}

fn uid(byte: u8) -> Hash32 {
    Hash32::new([byte; 32])
}

// ---------------------------------------------------------------------------
// cancel_all_orders
// ---------------------------------------------------------------------------

#[test]
fn cancel_all_on_empty_book_returns_zero() {
    let book = new_book();
    let result = book.cancel_all_orders();
    assert_eq!(result.cancelled_count(), 0);
    assert!(result.cancelled_order_ids().is_empty());
    assert!(result.is_empty());
}

#[test]
fn cancel_all_removes_every_order() {
    let book = new_book();

    for price in [90, 95, 100] {
        book.add_limit_order(Id::new_uuid(), price, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");
    }
    for price in [110, 115, 120] {
        book.add_limit_order(
            Id::new_uuid(),
            price,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("add ask");
    }

    let result = book.cancel_all_orders();

    assert_eq!(result.cancelled_count(), 6);
    assert_eq!(result.cancelled_order_ids().len(), 6);
    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), None);
}

#[test]
fn cancel_all_cleans_order_locations() {
    let book = new_book();
    let id = Id::new_uuid();
    book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("add");

    let _ = book.cancel_all_orders();
    assert_eq!(book.best_bid(), None);
}

// ---------------------------------------------------------------------------
// cancel_orders_by_side
// ---------------------------------------------------------------------------

#[test]
fn cancel_by_side_buy_leaves_asks() {
    let book = new_book();

    book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid");
    book.add_limit_order(Id::new_uuid(), 95, 5, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid 2");
    book.add_limit_order(Id::new_uuid(), 200, 8, Side::Sell, TimeInForce::Gtc, None)
        .expect("ask");

    let result = book.cancel_orders_by_side(Side::Buy);

    assert_eq!(result.cancelled_count(), 2);
    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), Some(200));
}

#[test]
fn cancel_by_side_sell_leaves_bids() {
    let book = new_book();

    book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid");
    book.add_limit_order(Id::new_uuid(), 200, 8, Side::Sell, TimeInForce::Gtc, None)
        .expect("ask");
    book.add_limit_order(Id::new_uuid(), 210, 3, Side::Sell, TimeInForce::Gtc, None)
        .expect("ask 2");

    let result = book.cancel_orders_by_side(Side::Sell);

    assert_eq!(result.cancelled_count(), 2);
    assert_eq!(book.best_bid(), Some(100));
    assert_eq!(book.best_ask(), None);
}

#[test]
fn cancel_by_side_on_empty_side() {
    let book = new_book();
    book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid");

    let result = book.cancel_orders_by_side(Side::Sell);
    assert!(result.is_empty());
    assert_eq!(book.best_bid(), Some(100));
}

// ---------------------------------------------------------------------------
// cancel_orders_by_user
// ---------------------------------------------------------------------------

#[test]
fn cancel_by_user_removes_only_matching_orders() {
    let book = new_book();
    let user_a = uid(1);
    let user_b = uid(2);

    let id_a1 = Id::new_uuid();
    let id_a2 = Id::new_uuid();
    let id_b1 = Id::new_uuid();

    book.add_limit_order_with_user(id_a1, 100, 10, Side::Buy, TimeInForce::Gtc, user_a, None)
        .expect("a1");
    book.add_limit_order_with_user(id_a2, 200, 5, Side::Sell, TimeInForce::Gtc, user_a, None)
        .expect("a2");
    book.add_limit_order_with_user(id_b1, 95, 20, Side::Buy, TimeInForce::Gtc, user_b, None)
        .expect("b1");

    let result = book.cancel_orders_by_user(user_a);
    assert_eq!(result.cancelled_count(), 2);
    assert!(result.cancelled_order_ids().contains(&id_a1));
    assert!(result.cancelled_order_ids().contains(&id_a2));

    // user_b order remains
    assert_eq!(book.best_bid(), Some(95));
    assert_eq!(book.best_ask(), None);
}

#[test]
fn cancel_by_user_no_match_returns_zero() {
    let book = new_book();
    let user_a = uid(1);
    let user_b = uid(2);

    book.add_limit_order_with_user(
        Id::new_uuid(),
        100,
        10,
        Side::Buy,
        TimeInForce::Gtc,
        user_a,
        None,
    )
    .expect("a1");

    let result = book.cancel_orders_by_user(user_b);
    assert!(result.is_empty());
    assert_eq!(book.best_bid(), Some(100));
}

#[test]
fn cancel_by_user_across_multiple_levels_and_sides() {
    let book = new_book();
    let user = uid(1);
    let other = uid(2);

    book.add_limit_order_with_user(
        Id::new_uuid(),
        100,
        10,
        Side::Buy,
        TimeInForce::Gtc,
        user,
        None,
    )
    .expect("buy 100");
    book.add_limit_order_with_user(
        Id::new_uuid(),
        95,
        5,
        Side::Buy,
        TimeInForce::Gtc,
        user,
        None,
    )
    .expect("buy 95");
    book.add_limit_order_with_user(
        Id::new_uuid(),
        200,
        8,
        Side::Sell,
        TimeInForce::Gtc,
        user,
        None,
    )
    .expect("sell 200");
    book.add_limit_order_with_user(
        Id::new_uuid(),
        90,
        20,
        Side::Buy,
        TimeInForce::Gtc,
        other,
        None,
    )
    .expect("other buy");

    let result = book.cancel_orders_by_user(user);
    assert_eq!(result.cancelled_count(), 3);
    assert_eq!(book.best_bid(), Some(90));
}

// ---------------------------------------------------------------------------
// cancel_orders_by_price_range
// ---------------------------------------------------------------------------

#[test]
fn cancel_by_price_range_inclusive_boundaries() {
    let book = new_book();

    let id1 = Id::new_uuid();
    let id2 = Id::new_uuid();
    let id3 = Id::new_uuid();

    book.add_limit_order(id1, 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("100");
    book.add_limit_order(id2, 200, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("200");
    book.add_limit_order(id3, 300, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("300");

    let result = book.cancel_orders_by_price_range(Side::Buy, 100, 200);
    assert_eq!(result.cancelled_count(), 2);
    assert!(result.cancelled_order_ids().contains(&id1));
    assert!(result.cancelled_order_ids().contains(&id2));
    assert_eq!(book.best_bid(), Some(300));
}

#[test]
fn cancel_by_price_range_single_price() {
    let book = new_book();

    let id = Id::new_uuid();
    book.add_limit_order(id, 150, 10, Side::Sell, TimeInForce::Gtc, None)
        .expect("add");
    book.add_limit_order(Id::new_uuid(), 200, 10, Side::Sell, TimeInForce::Gtc, None)
        .expect("add 2");

    let result = book.cancel_orders_by_price_range(Side::Sell, 150, 150);
    assert_eq!(result.cancelled_count(), 1);
    assert!(result.cancelled_order_ids().contains(&id));
    assert_eq!(book.best_ask(), Some(200));
}

#[test]
fn cancel_by_price_range_inverted_returns_zero() {
    let book = new_book();
    book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("add");

    let result = book.cancel_orders_by_price_range(Side::Buy, 200, 100);
    assert!(result.is_empty());
    assert_eq!(book.best_bid(), Some(100));
}

#[test]
fn cancel_by_price_range_no_orders_in_range() {
    let book = new_book();
    book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("add");

    let result = book.cancel_orders_by_price_range(Side::Buy, 200, 300);
    assert!(result.is_empty());
}

#[test]
fn cancel_by_price_range_multiple_orders_at_same_level() {
    let book = new_book();

    let id1 = Id::new_uuid();
    let id2 = Id::new_uuid();

    book.add_limit_order(id1, 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("add 1");
    book.add_limit_order(id2, 100, 20, Side::Buy, TimeInForce::Gtc, None)
        .expect("add 2");

    let result = book.cancel_orders_by_price_range(Side::Buy, 100, 100);
    assert_eq!(result.cancelled_count(), 2);
    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), None);
}

#[test]
fn cancel_by_price_range_on_wrong_side_returns_zero() {
    let book = new_book();
    book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("add bid");

    // Asks side has nothing at 100
    let result = book.cancel_orders_by_price_range(Side::Sell, 100, 100);
    assert!(result.is_empty());
    assert_eq!(book.best_bid(), Some(100));
}

// ---------------------------------------------------------------------------
// Mixed order types
// ---------------------------------------------------------------------------

#[test]
fn cancel_all_with_iceberg_orders() {
    let book = new_book();

    book.add_iceberg_order(
        Id::new_uuid(),
        100,
        5,
        15,
        Side::Buy,
        TimeInForce::Gtc,
        None,
    )
    .expect("iceberg");
    book.add_limit_order(Id::new_uuid(), 200, 10, Side::Sell, TimeInForce::Gtc, None)
        .expect("limit");

    let result = book.cancel_all_orders();
    assert_eq!(result.cancelled_count(), 2);
    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), None);
}

#[test]
fn cancel_all_with_post_only_orders() {
    let book = new_book();

    book.add_post_only_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("post-only");
    book.add_limit_order(Id::new_uuid(), 200, 10, Side::Sell, TimeInForce::Gtc, None)
        .expect("limit");

    let result = book.cancel_all_orders();
    assert_eq!(result.cancelled_count(), 2);
    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), None);
}

// ---------------------------------------------------------------------------
// MassCancelResult struct
// ---------------------------------------------------------------------------

#[test]
fn mass_cancel_result_default_is_empty() {
    let result = MassCancelResult::default();
    assert!(result.is_empty());
    assert_eq!(result.cancelled_count(), 0);
    assert!(result.cancelled_order_ids().is_empty());
}

#[test]
fn mass_cancel_result_display() {
    let result = MassCancelResult::default();
    let display = format!("{result}");
    assert!(display.contains("0"));
}

// ---------------------------------------------------------------------------
// STP-enabled book
// ---------------------------------------------------------------------------

#[test]
fn cancel_by_user_on_stp_enabled_book() {
    let mut book: OrderBook<()> = OrderBook::new("TEST");
    book.set_stp_mode(STPMode::CancelTaker);

    let user_a = uid(1);
    let user_b = uid(2);

    book.add_limit_order_with_user(
        Id::new_uuid(),
        100,
        10,
        Side::Buy,
        TimeInForce::Gtc,
        user_a,
        None,
    )
    .expect("a buy");
    book.add_limit_order_with_user(
        Id::new_uuid(),
        200,
        5,
        Side::Sell,
        TimeInForce::Gtc,
        user_b,
        None,
    )
    .expect("b sell");

    let result = book.cancel_orders_by_user(user_a);
    assert_eq!(result.cancelled_count(), 1);
    assert_eq!(book.best_ask(), Some(200));
}

// ---------------------------------------------------------------------------
// Sequential mass cancels
// ---------------------------------------------------------------------------

#[test]
fn double_cancel_all_is_idempotent() {
    let book = new_book();

    book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("add");

    let r1 = book.cancel_all_orders();
    assert_eq!(r1.cancelled_count(), 1);

    let r2 = book.cancel_all_orders();
    assert!(r2.is_empty());
}

#[test]
fn cancel_by_side_then_cancel_all() {
    let book = new_book();

    book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid");
    book.add_limit_order(Id::new_uuid(), 200, 5, Side::Sell, TimeInForce::Gtc, None)
        .expect("ask");

    let r1 = book.cancel_orders_by_side(Side::Buy);
    assert_eq!(r1.cancelled_count(), 1);

    let r2 = book.cancel_all_orders();
    assert_eq!(r2.cancelled_count(), 1); // only the ask remains
}

// ---------------------------------------------------------------------------
// user_orders tracking consistency (Issue #13)
// ---------------------------------------------------------------------------

#[test]
fn user_orders_populated_on_add_and_cleared_on_cancel() {
    let book = new_book();
    let user = uid(1);
    let id1 = Id::new_uuid();
    let id2 = Id::new_uuid();

    book.add_limit_order_with_user(id1, 100, 10, Side::Buy, TimeInForce::Gtc, user, None)
        .expect("add1");
    book.add_limit_order_with_user(id2, 101, 5, Side::Buy, TimeInForce::Gtc, user, None)
        .expect("add2");

    // cancel_orders_by_user should find both via the index
    let result = book.cancel_orders_by_user(user);
    assert_eq!(result.cancelled_count(), 2);
    assert!(result.cancelled_order_ids().contains(&id1));
    assert!(result.cancelled_order_ids().contains(&id2));

    // Second call should return 0 — index is clean
    let result2 = book.cancel_orders_by_user(user);
    assert_eq!(result2.cancelled_count(), 0);
}

#[test]
fn user_orders_cleaned_after_individual_cancel() {
    let book = new_book();
    let user = uid(1);
    let id1 = Id::new_uuid();
    let id2 = Id::new_uuid();

    book.add_limit_order_with_user(id1, 100, 10, Side::Buy, TimeInForce::Gtc, user, None)
        .expect("add1");
    book.add_limit_order_with_user(id2, 101, 5, Side::Buy, TimeInForce::Gtc, user, None)
        .expect("add2");

    // Cancel one order individually
    let _ = book.cancel_order(id1);

    // cancel_orders_by_user should only find the remaining one
    let result = book.cancel_orders_by_user(user);
    assert_eq!(result.cancelled_count(), 1);
    assert!(result.cancelled_order_ids().contains(&id2));
}

#[test]
fn user_orders_cleaned_after_full_match() {
    let book = new_book();
    let maker_user = uid(1);
    let maker_id = Id::new_uuid();

    // Place a resting sell order
    book.add_limit_order_with_user(
        maker_id,
        100,
        10,
        Side::Sell,
        TimeInForce::Gtc,
        maker_user,
        None,
    )
    .expect("maker");

    // Submit a buy that fully fills the maker
    let _ = book.submit_market_order(Id::new_uuid(), 10, Side::Buy);

    // The maker's order should be gone from the user_orders index
    let result = book.cancel_orders_by_user(maker_user);
    assert_eq!(result.cancelled_count(), 0);
}

#[test]
fn user_orders_partial_match_keeps_resting_order() {
    let book = new_book();
    let maker_user = uid(1);
    let maker_id = Id::new_uuid();

    // Place a resting sell order with qty 20
    book.add_limit_order_with_user(
        maker_id,
        100,
        20,
        Side::Sell,
        TimeInForce::Gtc,
        maker_user,
        None,
    )
    .expect("maker");

    // Partially fill 5 of the 20
    let _ = book.submit_market_order(Id::new_uuid(), 5, Side::Buy);

    // The remaining order should still be in the user_orders index
    let result = book.cancel_orders_by_user(maker_user);
    assert_eq!(result.cancelled_count(), 1);
    assert!(result.cancelled_order_ids().contains(&maker_id));
}

#[test]
fn multi_user_cancel_does_not_affect_other_users() {
    let book = new_book();
    let user_a = uid(1);
    let user_b = uid(2);

    let a1 = Id::new_uuid();
    let b1 = Id::new_uuid();
    let b2 = Id::new_uuid();

    book.add_limit_order_with_user(a1, 100, 10, Side::Buy, TimeInForce::Gtc, user_a, None)
        .expect("a1");
    book.add_limit_order_with_user(b1, 99, 5, Side::Buy, TimeInForce::Gtc, user_b, None)
        .expect("b1");
    book.add_limit_order_with_user(b2, 98, 3, Side::Buy, TimeInForce::Gtc, user_b, None)
        .expect("b2");

    // Cancel user_a
    let ra = book.cancel_orders_by_user(user_a);
    assert_eq!(ra.cancelled_count(), 1);

    // user_b's orders should still be there
    let rb = book.cancel_orders_by_user(user_b);
    assert_eq!(rb.cancelled_count(), 2);
}

#[test]
fn cancel_all_clears_all_user_entries() {
    let book = new_book();
    let user_a = uid(1);
    let user_b = uid(2);

    book.add_limit_order_with_user(
        Id::new_uuid(),
        100,
        10,
        Side::Buy,
        TimeInForce::Gtc,
        user_a,
        None,
    )
    .expect("a");
    book.add_limit_order_with_user(
        Id::new_uuid(),
        200,
        5,
        Side::Sell,
        TimeInForce::Gtc,
        user_b,
        None,
    )
    .expect("b");

    let _ = book.cancel_all_orders();

    // Both users should have empty indices now
    assert_eq!(book.cancel_orders_by_user(user_a).cancelled_count(), 0);
    assert_eq!(book.cancel_orders_by_user(user_b).cancelled_count(), 0);
}

#[test]
fn stp_cancel_maker_cleans_user_orders() {
    let book: OrderBook<()> = OrderBook::with_stp_mode("STP", STPMode::CancelMaker);
    let user = uid(1);

    // Place a resting sell order
    let maker_id = Id::new_uuid();
    book.add_limit_order_with_user(maker_id, 100, 10, Side::Sell, TimeInForce::Gtc, user, None)
        .expect("maker");

    // Same user submits a buy that would self-trade — CancelMaker removes the resting order
    let taker_id = Id::new_uuid();
    let _ =
        book.add_limit_order_with_user(taker_id, 100, 10, Side::Buy, TimeInForce::Gtc, user, None);

    // The cancelled maker should be removed from user_orders.
    // The taker may or may not rest depending on matching, but the
    // maker should definitely be gone.
    let result = book.cancel_orders_by_user(user);
    // The maker was cancelled by STP; the taker rests (if not fully filled).
    // We just verify the maker_id is NOT in the remaining user_orders.
    assert!(!result.cancelled_order_ids().contains(&maker_id));
}

// ---------------------------------------------------------------------------
// Optimised cancel_all_orders (Issue #14)
// ---------------------------------------------------------------------------

#[test]
fn cancel_all_emits_price_level_changed_events() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let event_count = Arc::new(AtomicUsize::new(0));
    let counter = event_count.clone();

    let mut book: OrderBook<()> = OrderBook::new("EVENTS");
    book.set_price_level_listener(Arc::new(move |_event| {
        counter.fetch_add(1, Ordering::Relaxed);
    }));

    // Create 3 distinct price levels on bids, 2 on asks = 5 levels total
    for i in 0..3 {
        book.add_limit_order(
            Id::new_uuid(),
            100 + i,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        )
        .expect("bid");
    }
    for i in 0..2 {
        book.add_limit_order(
            Id::new_uuid(),
            200 + i,
            5,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("ask");
    }

    // Reset counter — we only care about events from cancel_all
    event_count.store(0, Ordering::Relaxed);

    let result = book.cancel_all_orders();
    assert_eq!(result.cancelled_count(), 5);

    // Should have fired exactly 5 events (one per price level)
    assert_eq!(event_count.load(Ordering::Relaxed), 5);
}

#[test]
fn cancel_all_returns_all_ids() {
    let book = new_book();
    let mut ids = Vec::new();

    // Use non-crossing prices: bids at 50..60, asks at 200..210
    for i in 0..10 {
        let id = Id::new_uuid();
        ids.push(id);
        book.add_limit_order(id, 50 + i, 1, Side::Buy, TimeInForce::Gtc, None)
            .expect("bid");
    }
    for i in 0..10 {
        let id = Id::new_uuid();
        ids.push(id);
        book.add_limit_order(id, 200 + i, 1, Side::Sell, TimeInForce::Gtc, None)
            .expect("ask");
    }

    let result = book.cancel_all_orders();
    assert_eq!(result.cancelled_count(), 20);
    for id in &ids {
        assert!(result.cancelled_order_ids().contains(id));
    }
}

#[test]
fn cancel_all_idempotent() {
    let book = new_book();
    for _ in 0..5 {
        book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("add");
    }

    let r1 = book.cancel_all_orders();
    assert_eq!(r1.cancelled_count(), 5);

    let r2 = book.cancel_all_orders();
    assert_eq!(r2.cancelled_count(), 0);
    assert!(r2.is_empty());
}

#[test]
fn cancel_all_clears_book_completely() {
    let book = new_book();
    for i in 0..100 {
        let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
        book.add_limit_order(
            Id::new_uuid(),
            100 + (i % 50),
            10,
            side,
            TimeInForce::Gtc,
            None,
        )
        .expect("add");
    }

    let _ = book.cancel_all_orders();

    assert_eq!(book.best_bid(), None);
    assert_eq!(book.best_ask(), None);
}

// ---------------------------------------------------------------------------
// Cross-instance result determinism (Issue #190)
//
// The mass-cancel result order must be identical across independently
// constructed books fed the same insertion sequence. Before the #190 fix,
// `cancel_all_orders` collected ids from the `order_locations` DashMap, whose
// hasher is seeded per instance, so two books with the same orders produced
// different `cancelled_order_ids` orderings — and thus divergent journaled
// `SequencerResult::MassCancelled` payloads. This is the regression guard.
// ---------------------------------------------------------------------------

/// A deterministic, interleaved admission plan: two price levels per side and
/// two orders per level, admitted in an order that scrambles DashMap insertion
/// relative to the price-then-insertion-sequence contract order. The `Id`s are
/// generated once and shared, so every book built from the plan holds the
/// identical order set.
fn interleaved_plan() -> Vec<(Id, u128, Side)> {
    vec![
        (Id::new_uuid(), 100, Side::Buy),  // b100a
        (Id::new_uuid(), 120, Side::Sell), // a120a
        (Id::new_uuid(), 90, Side::Buy),   // b90a
        (Id::new_uuid(), 110, Side::Sell), // a110a
        (Id::new_uuid(), 100, Side::Buy),  // b100b
        (Id::new_uuid(), 110, Side::Sell), // a110b
        (Id::new_uuid(), 90, Side::Buy),   // b90b
        (Id::new_uuid(), 120, Side::Sell), // a120b
    ]
}

fn book_from_plan(plan: &[(Id, u128, Side)]) -> OrderBook<()> {
    let book = new_book();
    for &(id, price, side) in plan {
        book.add_limit_order(id, price, 1, side, TimeInForce::Gtc, None)
            .expect("add");
    }
    book
}

/// Build `instances` independent books from the same plan, run `op` on each,
/// and return every result's id vector. Independent books seed independent
/// DashMap hashers, so any hasher-order dependence surfaces as divergence.
fn ids_across_instances<F>(plan: &[(Id, u128, Side)], instances: usize, op: F) -> Vec<Vec<Id>>
where
    F: Fn(&OrderBook<()>) -> MassCancelResult,
{
    (0..instances)
        .map(|_| {
            let book = book_from_plan(plan);
            op(&book).cancelled_order_ids().to_vec()
        })
        .collect()
}

fn assert_all_identical(runs: &[Vec<Id>]) {
    let first = runs.first().expect("at least one run");
    for (i, run) in runs.iter().enumerate() {
        assert_eq!(run, first, "instance {i} diverged from instance 0");
    }
}

#[test]
fn test_cancel_all_orders_cross_instance_returns_identical_ids() {
    let plan = interleaved_plan();
    let runs = ids_across_instances(&plan, 8, |book| book.cancel_all_orders());
    assert_eq!(runs[0].len(), 8);
    assert_all_identical(&runs);
}

#[test]
fn test_cancel_orders_by_side_cross_instance_returns_identical_ids() {
    let plan = interleaved_plan();
    let buy_runs = ids_across_instances(&plan, 8, |book| book.cancel_orders_by_side(Side::Buy));
    assert_eq!(buy_runs[0].len(), 4);
    assert_all_identical(&buy_runs);

    let sell_runs = ids_across_instances(&plan, 8, |book| book.cancel_orders_by_side(Side::Sell));
    assert_eq!(sell_runs[0].len(), 4);
    assert_all_identical(&sell_runs);
}

#[test]
fn test_cancel_orders_by_price_range_cross_instance_returns_identical_ids() {
    let plan = interleaved_plan();
    let runs = ids_across_instances(&plan, 8, |book| {
        book.cancel_orders_by_price_range(Side::Buy, 90, 100)
    });
    assert_eq!(runs[0].len(), 4);
    assert_all_identical(&runs);
}

#[test]
fn test_cancel_orders_by_user_cross_instance_returns_identical_ids() {
    // cancel_orders_by_user was already deterministic (admission-history order);
    // this pins that it stays stable across independent instances.
    let user = uid(7);
    let ids: Vec<Id> = (0..6).map(|_| Id::new_uuid()).collect();
    let prices = [100u128, 120, 90, 110, 100, 90];
    let sides = [
        Side::Buy,
        Side::Sell,
        Side::Buy,
        Side::Sell,
        Side::Buy,
        Side::Buy,
    ];

    let build = || {
        let book = new_book();
        for i in 0..ids.len() {
            book.add_limit_order_with_user(
                ids[i],
                prices[i],
                1,
                sides[i],
                TimeInForce::Gtc,
                user,
                None,
            )
            .expect("add");
        }
        book.cancel_orders_by_user(user)
            .cancelled_order_ids()
            .to_vec()
    };

    let runs: Vec<Vec<Id>> = (0..8).map(|_| build()).collect();
    // Admission-history order: exactly the sequence the ids were added in.
    assert_eq!(runs[0], ids);
    assert_all_identical(&runs);
}
