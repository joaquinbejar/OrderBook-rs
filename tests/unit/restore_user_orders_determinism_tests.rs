//! Cross-instance determinism of `cancel_orders_by_user` after a snapshot
//! restore (Issue #192).
//!
//! `restore_from_snapshot_package` rebuilds the `user_orders`
//! (`DashMap<Hash32, Vec<Id>>`) index by walking the restored resting orders.
//! Before #192 that walk used the `DashMap`-backed, order-unstable
//! `iter_orders()` view, so each freshly constructed book (its own per-instance
//! hasher) rebuilt the per-user `Vec<Id>` in a different order. A
//! `cancel_orders_by_user` issued after the restore then returned a
//! `cancelled_order_ids` sequence that diverged across restores of the *same*
//! package — which would break replay if that payload were journaled.
//!
//! The fix walks price levels in a fixed price-then-insertion-sequence order
//! (bids ascending price, then asks ascending price; within each level ascending
//! insertion sequence via `PriceLevel::snapshot_by_seq_into`), so the rebuild —
//! and therefore the by-user cancel — is byte-identical across every restore of
//! the same package. It is NOT the original admission history (a snapshot cannot
//! recover that), but it is deterministic. These tests are the regression guard.

use orderbook_rs::OrderBook;
use pricelevel::{Hash32, Id, Side, TimeInForce};

/// User A — the user whose orders we assert on. Spread across two bid levels
/// (one with two orders) and two ask levels (one with two orders).
fn user_a() -> Hash32 {
    Hash32::new([0xAA; 32])
}

/// User B — interleaving noise so the admission stream, and hence the
/// `DashMap` insertion order, is scrambled relative to the price-then-seq
/// contract order.
fn user_b() -> Hash32 {
    Hash32::new([0xBB; 32])
}

/// Ids for user A's six orders, labelled `<side><price>[_n]`. Within a shared
/// level, the lower suffix is admitted first (so ascending insertion sequence
/// at that level is `_1` then `_2`).
struct UserAOrders {
    b90: Id,
    b100_1: Id,
    b100_2: Id,
    a110: Id,
    a120_1: Id,
    a120_2: Id,
}

/// Build a book, admit user A's and user B's orders in a deliberately scrambled
/// order across sides and levels, and return the book plus user A's ids.
///
/// The intra-level relative admission order of A's paired orders is preserved
/// (`b100_1` before `b100_2`, `a120_1` before `a120_2`) so the expected
/// price-then-insertion-sequence order is unambiguous.
fn build_scrambled_book() -> (OrderBook<()>, UserAOrders) {
    let book: OrderBook<()> = OrderBook::new("TEST");
    let a = user_a();
    let b = user_b();

    let ids = UserAOrders {
        b90: Id::new_uuid(),
        b100_1: Id::new_uuid(),
        b100_2: Id::new_uuid(),
        a110: Id::new_uuid(),
        a120_1: Id::new_uuid(),
        a120_2: Id::new_uuid(),
    };

    let b_b90 = Id::new_uuid();
    let b_b100 = Id::new_uuid();
    let b_a110 = Id::new_uuid();
    let b_a120 = Id::new_uuid();

    let add = |id: Id, price: u128, side: Side, user: Hash32| {
        assert!(
            book.add_limit_order_with_user(id, price, 1, side, TimeInForce::Gtc, user, None)
                .is_ok(),
            "add order at price {price}"
        );
    };

    // Interleaved admission across users, sides, and levels. The admission
    // sequence is intentionally unrelated to the price-then-seq contract order,
    // but A's paired orders keep their relative order within each level.
    add(ids.b100_1, 100, Side::Buy, a);
    add(b_a120, 120, Side::Sell, b);
    add(ids.a120_1, 120, Side::Sell, a);
    add(b_b90, 90, Side::Buy, b);
    add(ids.b90, 90, Side::Buy, a);
    add(ids.a110, 110, Side::Sell, a);
    add(b_b100, 100, Side::Buy, b);
    add(ids.b100_2, 100, Side::Buy, a);
    add(b_a110, 110, Side::Sell, b);
    add(ids.a120_2, 120, Side::Sell, a);

    (book, ids)
}

/// The documented order a post-restore `cancel_orders_by_user(user_a)` returns:
/// bids ascending price, then asks ascending price; within each level ascending
/// insertion sequence (oldest first).
fn expected_user_a_order(ids: &UserAOrders) -> Vec<Id> {
    vec![
        ids.b90,    // bid 90
        ids.b100_1, // bid 100, oldest
        ids.b100_2, // bid 100, next
        ids.a110,   // ask 110
        ids.a120_1, // ask 120, oldest
        ids.a120_2, // ask 120, next
    ]
}

/// Restore the same JSON-serialized package into `n` freshly constructed books
/// (each with its own randomised `DashMap` hasher) and return each restored
/// book's `cancel_orders_by_user(user_a)` id vector.
fn restore_and_cancel_by_user(json: &str, n: usize) -> Vec<Vec<Id>> {
    let mut results = Vec::with_capacity(n);
    for _ in 0..n {
        let mut restored: OrderBook<()> = OrderBook::new("TEST");
        match restored.restore_from_snapshot_json(json) {
            Ok(()) => {}
            Err(e) => panic!("restore must succeed: {e}"),
        }
        let result = restored.cancel_orders_by_user(user_a());
        results.push(result.cancelled_order_ids().to_vec());
    }
    results
}

/// #192: restoring the same package into several fresh books and cancelling by
/// user must yield a byte-identical `cancelled_order_ids` vector every time.
///
/// Each restore targets an independently constructed book, so its `user_orders`
/// `DashMap` is seeded with a distinct hasher. If the rebuild observed that
/// hasher's iteration order (the pre-#192 `iter_orders` path), the vectors would
/// disagree across restores with high probability. With the price-then-seq
/// rebuild they must all be equal.
#[test]
fn test_restore_then_cancel_by_user_cross_instance_returns_identical_order() {
    let (book, ids) = build_scrambled_book();
    let json = match book.snapshot_to_json(usize::MAX) {
        Ok(j) => j,
        Err(e) => panic!("snapshot_to_json must succeed: {e}"),
    };

    let results = restore_and_cancel_by_user(&json, 8);

    // Every restore cancelled exactly user A's six orders.
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.len(), 6, "restore {i} cancelled the wrong count");
    }

    // All restores agree byte-for-byte with the first.
    let first = &results[0];
    for (i, r) in results.iter().enumerate().skip(1) {
        assert_eq!(
            r, first,
            "restore {i} produced a different by-user cancel order than restore 0 — \
             user_orders rebuild is not deterministic across instances"
        );
    }

    // And the agreed order is the documented price-then-seq order, not merely
    // some arbitrary-but-stable permutation.
    assert_eq!(*first, expected_user_a_order(&ids));
}

/// #192: the restored book's by-user cancel returns the exact documented
/// price-then-insertion-sequence order — bids ascending, then asks ascending,
/// oldest-first within each level.
#[test]
fn test_restore_then_cancel_by_user_returns_price_then_seq_order() {
    let (book, ids) = build_scrambled_book();
    let json = match book.snapshot_to_json(usize::MAX) {
        Ok(j) => j,
        Err(e) => panic!("snapshot_to_json must succeed: {e}"),
    };

    let mut restored: OrderBook<()> = OrderBook::new("TEST");
    match restored.restore_from_snapshot_json(&json) {
        Ok(()) => {}
        Err(e) => panic!("restore must succeed: {e}"),
    }

    let result = restored.cancel_orders_by_user(user_a());
    assert_eq!(result.cancelled_count(), 6);
    assert_eq!(
        result.cancelled_order_ids(),
        expected_user_a_order(&ids).as_slice(),
    );

    // User B's orders are untouched by the user-A cancel.
    assert_eq!(
        restored.cancel_orders_by_user(user_b()).cancelled_count(),
        4
    );
}
