//! Integration tests for time-in-force eviction (`evict_expired_orders`).
//!
//! Covers the explicit sweep that removes resting `Gtd` / `Day` orders after
//! their deadline (issue #189). Resting expiry is *not* checked on the matching
//! hot path — the sweep is the only eviction point — so these tests assert the
//! deterministic output order, the `deadline == now` boundary, idempotence,
//! event emission, order-tracker reason, and manager parity.

use orderbook_rs::orderbook::manager::{BookManager, BookManagerStd, BookManagerTokio};
use orderbook_rs::orderbook::order_state::{CancelReason, OrderStatus};
use orderbook_rs::{Clock, OrderBook, StubClock};
use pricelevel::{Id, Side, TimeInForce, TimestampMs};
use std::sync::{Arc, Mutex};

/// A book whose clock starts at logical `0` so small `Gtd` deadlines are
/// admitted (wall-clock admission would treat them as already expired) and the
/// caller-supplied sweep timestamp is what drives expiry.
fn expiring_book(symbol: &str) -> OrderBook<()> {
    OrderBook::with_clock(
        symbol,
        Arc::new(StubClock::starting_at(0)) as Arc<dyn Clock>,
    )
}

#[test]
fn expired_gtd_is_evicted_and_no_longer_matchable() {
    let book = expiring_book("TEST");
    let gtd = Id::new_uuid();
    book.add_limit_order(gtd, 100, 10, Side::Sell, TimeInForce::Gtd(1_000), None)
        .expect("add gtd");

    let evicted = book.evict_expired_orders(TimestampMs::new(1_000));
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].id(), gtd);
    assert_eq!(book.best_ask(), None);

    // A crossing buy now finds no liquidity.
    let taker = Id::new_uuid();
    assert!(book.match_market_order(taker, 10, Side::Buy).is_err());
}

#[test]
fn unexpired_gtd_and_gtc_are_untouched() {
    let book = expiring_book("TEST");
    let gtc = Id::new_uuid();
    let gtd_future = Id::new_uuid();
    book.add_limit_order(gtc, 100, 10, Side::Buy, TimeInForce::Gtc, None)
        .expect("gtc");
    book.add_limit_order(gtd_future, 99, 5, Side::Buy, TimeInForce::Gtd(10_000), None)
        .expect("gtd future");

    let evicted = book.evict_expired_orders(TimestampMs::new(5_000));
    assert!(evicted.is_empty());
    assert_eq!(book.best_bid(), Some(100));
}

#[test]
fn boundary_deadline_equals_now_is_expired() {
    let book = expiring_book("TEST");
    let id = Id::new_uuid();
    book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
        .expect("add");

    // deadline - 1: not expired.
    assert!(book.evict_expired_orders(TimestampMs::new(999)).is_empty());
    // deadline exactly: expired (matches is_expired's `now >= deadline`).
    assert_eq!(book.evict_expired_orders(TimestampMs::new(1_000)).len(), 1);
}

#[test]
fn deterministic_order_bids_then_asks_ascending_fifo() {
    let book = expiring_book("TEST");

    // Bids across two levels; FIFO within the 95 level.
    let b95a = Id::new_uuid();
    let b95b = Id::new_uuid();
    let b90 = Id::new_uuid();
    book.add_limit_order(b95a, 95, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
        .expect("b95a");
    book.add_limit_order(b95b, 95, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
        .expect("b95b");
    book.add_limit_order(b90, 90, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
        .expect("b90");

    // Asks across two levels.
    let a100 = Id::new_uuid();
    let a110 = Id::new_uuid();
    book.add_limit_order(a100, 100, 1, Side::Sell, TimeInForce::Gtd(1_000), None)
        .expect("a100");
    book.add_limit_order(a110, 110, 1, Side::Sell, TimeInForce::Gtd(1_000), None)
        .expect("a110");

    let ids: Vec<Id> = book
        .evict_expired_orders(TimestampMs::new(2_000))
        .iter()
        .map(|o| o.id())
        .collect();

    // Contract: bids ascending (90, then the 95 level FIFO), then asks ascending.
    assert_eq!(ids, vec![b90, b95a, b95b, a100, a110]);
}

#[test]
fn second_sweep_at_same_now_is_idempotent() {
    let book = expiring_book("TEST");
    let id = Id::new_uuid();
    book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
        .expect("add");

    assert_eq!(book.evict_expired_orders(TimestampMs::new(1_000)).len(), 1);
    assert!(
        book.evict_expired_orders(TimestampMs::new(1_000))
            .is_empty()
    );
}

#[test]
fn eviction_fires_book_change_event_for_touched_level() {
    use orderbook_rs::orderbook::book_change_event::PriceLevelChangedEvent;

    let events: Arc<Mutex<Vec<PriceLevelChangedEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let mut book = expiring_book("TEST");
    book.set_price_level_listener(Arc::new(move |ev: PriceLevelChangedEvent| {
        if let Ok(mut v) = sink.lock() {
            v.push(ev);
        }
    }));

    let id = Id::new_uuid();
    book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
        .expect("add");

    assert_eq!(book.evict_expired_orders(TimestampMs::new(1_000)).len(), 1);

    let recorded = events.lock().expect("lock");
    assert!(
        recorded
            .iter()
            .any(|ev| ev.side == Side::Buy && ev.price == 100 && ev.quantity == 0),
        "expected a qty->0 level change for the evicted level"
    );
}

#[test]
fn order_tracker_records_time_in_force_expired() {
    use orderbook_rs::orderbook::order_state::OrderStateTracker;

    let mut book = expiring_book("TEST");
    book.set_order_state_tracker(OrderStateTracker::new());

    let id = Id::new_uuid();
    book.add_limit_order(id, 100, 10, Side::Buy, TimeInForce::Gtd(1_000), None)
        .expect("add");

    assert_eq!(book.evict_expired_orders(TimestampMs::new(1_000)).len(), 1);

    let status = book
        .order_state_tracker()
        .and_then(|t| t.get(id))
        .expect("status present");
    assert!(matches!(
        status,
        OrderStatus::Cancelled {
            reason: CancelReason::TimeInForceExpired,
            ..
        }
    ));
}

#[test]
fn manager_std_per_symbol_and_all_books_parity() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("BTC/USD").expect("add BTC");
    mgr.add_book("ETH/USD").expect("add ETH");

    // Swap in a logical clock per book so small Gtd deadlines admit.
    if let Some(book) = mgr.get_book_mut("BTC/USD") {
        book.set_clock(Arc::new(StubClock::starting_at(0)) as Arc<dyn Clock>);
    }
    if let Some(book) = mgr.get_book_mut("ETH/USD") {
        book.set_clock(Arc::new(StubClock::starting_at(0)) as Arc<dyn Clock>);
    }

    let btc_id = Id::new_uuid();
    let eth_id = Id::new_uuid();
    if let Some(book) = mgr.get_book("BTC/USD") {
        book.add_limit_order(btc_id, 100, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("btc order");
    }
    if let Some(book) = mgr.get_book("ETH/USD") {
        book.add_limit_order(eth_id, 100, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("eth order");
    }

    // Per-symbol pass-through.
    let btc_evicted = mgr
        .evict_expired_orders("BTC/USD", TimestampMs::new(1_000))
        .expect("BTC managed");
    assert_eq!(btc_evicted.len(), 1);
    assert_eq!(btc_evicted[0].id(), btc_id);
    // Unknown symbol -> None.
    assert!(
        mgr.evict_expired_orders("NOPE", TimestampMs::new(1_000))
            .is_none()
    );

    // All-books variant covers the remaining book.
    let all = mgr.evict_expired_across_books(TimestampMs::new(1_000));
    assert_eq!(all.get("ETH/USD").map(|v| v.len()), Some(1));
    // BTC was already swept, so nothing left there.
    assert_eq!(all.get("BTC/USD").map(|v| v.len()), Some(0));
}

#[test]
fn manager_tokio_per_symbol_and_all_books_parity() {
    let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
    mgr.add_book("BTC/USD").expect("add BTC");

    if let Some(book) = mgr.get_book_mut("BTC/USD") {
        book.set_clock(Arc::new(StubClock::starting_at(0)) as Arc<dyn Clock>);
    }

    let id = Id::new_uuid();
    if let Some(book) = mgr.get_book("BTC/USD") {
        book.add_limit_order(id, 100, 1, Side::Buy, TimeInForce::Gtd(1_000), None)
            .expect("order");
    }

    let evicted = mgr
        .evict_expired_orders("BTC/USD", TimestampMs::new(1_000))
        .expect("managed");
    assert_eq!(evicted.len(), 1);

    // Idempotent across the all-books variant too.
    let all = mgr.evict_expired_across_books(TimestampMs::new(1_000));
    assert_eq!(all.get("BTC/USD").map(|v| v.len()), Some(0));
}
