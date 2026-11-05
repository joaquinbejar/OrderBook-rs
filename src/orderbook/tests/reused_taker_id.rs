//! A match or market taker reusing a live id is refused (#321).

use crate::OrderBookError;
use crate::orderbook::book::OrderBook;
use pricelevel::{
    Hash32, Id, MatchResult, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs,
};

type Book = OrderBook<()>;

fn id(n: u64) -> Id {
    Id::from_u64(n)
}

fn executed(result: MatchResult) -> u64 {
    result.executed_quantity().map_or(0, |q| q.as_u64())
}

fn limit(n: u64, price: u128, quantity: u64, side: Side) -> OrderType<()> {
    OrderType::Standard {
        id: id(n),
        price: Price::new(price),
        quantity: Quantity::new(quantity),
        side,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(0),
        time_in_force: TimeInForce::Gtc,
        extra_fields: (),
    }
}

/// Asks at 100 (id 1) and 101 (id 2), bid at 90 (id 3).
fn book() -> Book {
    let book = Book::new("TEST");
    book.add_order(limit(1, 100, 10, Side::Sell))
        .expect("ask 100");
    book.add_order(limit(2, 101, 10, Side::Sell))
        .expect("ask 101");
    book.add_order(limit(3, 90, 10, Side::Buy)).expect("bid 90");
    book
}

/// Every match and market entry point, as `(name, call)`.
type Entry = (
    &'static str,
    fn(&Book, Id, Side) -> Result<u64, OrderBookError>,
);

fn entry_points() -> Vec<Entry> {
    vec![
        ("match_order", |b, i, s| {
            let p = if s == Side::Buy { 101 } else { 90 };
            b.match_order(i, s, 5, Some(p)).map(executed)
        }),
        ("match_order_with_user", |b, i, s| {
            let p = if s == Side::Buy { 101 } else { 90 };
            b.match_order_with_user(i, s, 5, Some(p), Hash32::zero())
                .map(executed)
        }),
        ("match_limit_order", |b, i, s| {
            let p = if s == Side::Buy { 101 } else { 90 };
            b.match_limit_order(i, 5, s, p).map(executed)
        }),
        ("match_market_order", |b, i, s| {
            b.match_market_order(i, 5, s).map(executed)
        }),
        ("match_market_order_by_amount", |b, i, s| {
            b.match_market_order_by_amount(i, 505, s).map(executed)
        }),
        ("submit_market_order", |b, i, s| {
            b.submit_market_order(i, 5, s).map(executed)
        }),
        ("submit_market_order_with_user", |b, i, s| {
            b.submit_market_order_with_user(i, 5, s, Hash32::zero())
                .map(executed)
        }),
        ("submit_market_order_by_amount", |b, i, s| {
            b.submit_market_order_by_amount(i, 505, s).map(executed)
        }),
        ("submit_market_order_with_committed", |b, i, s| {
            b.submit_market_order_with_committed(i, 5, s)
                .map(executed)
                .map_err(|f| f.error)
        }),
        ("submit_market_order_by_amount_with_committed", |b, i, s| {
            b.submit_market_order_by_amount_with_committed(i, 505, s)
                .map(executed)
                .map_err(|f| f.error)
        }),
        ("match_orders_batch", |b, i, s| {
            let p = if s == Side::Buy { 101 } else { 90 };
            b.match_orders_batch(&[(i, s, 5, Some(p))])
                .remove(0)
                .map(executed)
        }),
    ]
}

fn assert_refused_untouched(reused: u64, side: Side) {
    for (name, call) in entry_points() {
        let book = book();
        let status_before = book.order_status(id(reused));
        let err = call(&book, id(reused), side).expect_err(name);
        assert!(
            matches!(err, OrderBookError::DuplicateOrderId { order_id } if order_id == id(reused)),
            "{name}: {err:?}"
        );
        assert_eq!(book.best_ask(), Some(100), "{name}");
        assert_eq!(book.best_bid(), Some(90), "{name}");
        assert_eq!(
            book.visible_quantity_at_price(100, Side::Sell),
            Some(10),
            "{name}"
        );
        assert_eq!(
            book.visible_quantity_at_price(101, Side::Sell),
            Some(10),
            "{name}"
        );
        assert_eq!(
            book.visible_quantity_at_price(90, Side::Buy),
            Some(10),
            "{name}"
        );
        assert_eq!(book.last_trade_price(), None, "{name}");
        assert_eq!(book.order_status(id(reused)), status_before, "{name}");
        assert!(book.get_order(id(reused)).is_some(), "{name}");
    }
}

#[test]
fn test_opposite_side_reuse_is_refused_on_every_entry_point() {
    assert_refused_untouched(1, Side::Buy);
}

#[test]
fn test_same_side_reuse_is_refused_on_every_entry_point() {
    assert_refused_untouched(2, Side::Sell);
    assert_refused_untouched(1, Side::Sell);
}

#[test]
fn test_fresh_id_fills_at_the_best_price_on_every_entry_point() {
    for (name, call) in entry_points() {
        let book = book();
        let executed = call(&book, id(99), Side::Buy).expect(name);
        assert_eq!(executed, 5, "{name}");
        assert_eq!(book.last_trade_price(), Some(100), "{name}");
        assert_eq!(
            book.visible_quantity_at_price(100, Side::Sell),
            Some(5),
            "{name}"
        );
        assert_eq!(
            book.visible_quantity_at_price(101, Side::Sell),
            Some(10),
            "{name}"
        );
    }
}

#[test]
fn test_id_is_reusable_once_the_order_is_gone() {
    let book = book();
    book.cancel_order(id(1)).expect("cancel");
    let result = book
        .submit_market_order(id(1), 5, Side::Buy)
        .expect("id is free");
    assert_eq!(executed(result), 5);
    assert_eq!(book.last_trade_price(), Some(101));
}
