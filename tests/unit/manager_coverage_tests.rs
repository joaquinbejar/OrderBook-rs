/******************************************************************************
   Unit tests for BookManager coverage gaps.
   Covers: BookManagerStd, BookManagerTokio trait methods,
   Default impls, start_trade_processor, error paths.
******************************************************************************/

use orderbook_rs::orderbook::manager::{BookManager, BookManagerStd, BookManagerTokio};
use pricelevel::{Hash32, Id, Side, TimeInForce};

// ─── BookManagerStd ─────────────────────────────────────────────────────────

#[test]
fn std_default_creates_empty_manager() {
    let mgr: BookManagerStd<()> = BookManagerStd::default();
    assert_eq!(mgr.book_count(), 0);
    assert!(mgr.symbols().is_empty());
}

#[test]
fn std_add_and_get_book() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("BTC/USD").expect("add book");
    assert!(mgr.has_book("BTC/USD"));
    assert!(!mgr.has_book("ETH/USD"));
    assert_eq!(mgr.book_count(), 1);

    let symbols = mgr.symbols();
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0], "BTC/USD");
}

#[test]
fn std_get_book_returns_none_for_unknown() {
    let mgr: BookManagerStd<()> = BookManagerStd::new();
    assert!(mgr.get_book("UNKNOWN").is_none());
}

#[test]
fn std_get_book_mut_returns_none_for_unknown() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    assert!(mgr.get_book_mut("UNKNOWN").is_none());
}

#[test]
fn std_get_book_returns_valid_ref() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("ETH/USD").expect("add book");
    let book = mgr.get_book("ETH/USD");
    assert!(book.is_some());
}

#[test]
fn std_get_book_mut_allows_modification() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("ETH/USD").expect("add book");
    let book = mgr.get_book_mut("ETH/USD").expect("book must exist");
    let _ = book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let snap = book.create_snapshot(usize::MAX);
    assert_eq!(snap.bids.len(), 1);
}

#[test]
fn std_remove_book_returns_some_when_exists() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("BTC/USD").expect("add book");
    let removed = mgr.remove_book("BTC/USD");
    assert!(removed.is_some());
    assert_eq!(mgr.book_count(), 0);
    assert!(!mgr.has_book("BTC/USD"));
}

#[test]
fn std_remove_book_returns_none_when_missing() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    let removed = mgr.remove_book("MISSING");
    assert!(removed.is_none());
}

#[test]
fn std_start_trade_processor_ok_first_time() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    let handle = mgr
        .start_trade_processor()
        .expect("should succeed first time");
    drop(mgr);
    handle
        .join()
        .expect("trade processor thread should join cleanly");
}

#[test]
fn std_start_trade_processor_fails_second_time() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    let handle = mgr
        .start_trade_processor()
        .expect("should succeed first time");
    let result = mgr.start_trade_processor();
    assert!(result.is_err());
    drop(mgr);
    handle
        .join()
        .expect("trade processor thread should join cleanly");
}

#[test]
fn std_add_order_and_cancel_across_books() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("BTC/USD").expect("add book");
    mgr.add_book("ETH/USD").expect("add book");

    let btc = mgr.get_book("BTC/USD").expect("BTC book must exist");
    let _ = btc.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let eth = mgr.get_book("ETH/USD").expect("ETH book must exist");
    let _ = eth.add_limit_order(Id::new_uuid(), 200, 5, Side::Sell, TimeInForce::Gtc, None);

    let results = mgr.cancel_all_across_books();
    assert_eq!(results.len(), 2);
    assert_eq!(results["BTC/USD"].cancelled_count(), 1);
    assert_eq!(results["ETH/USD"].cancelled_count(), 1);

    let btc_snap = mgr
        .get_book("BTC/USD")
        .expect("book")
        .create_snapshot(usize::MAX);
    assert!(btc_snap.bids.is_empty());
    let eth_snap = mgr
        .get_book("ETH/USD")
        .expect("book")
        .create_snapshot(usize::MAX);
    assert!(eth_snap.asks.is_empty());
}

#[test]
fn std_cancel_by_user_across_books() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("BTC/USD").expect("add book");

    let book = mgr.get_book("BTC/USD").expect("book must exist");
    let _ = book.add_limit_order_with_user(
        Id::new_uuid(),
        100,
        10,
        Side::Buy,
        TimeInForce::Gtc,
        Hash32::from([42u8; 32]),
        None,
    );

    let results = mgr.cancel_by_user_across_books(Hash32::from([42u8; 32]));
    assert_eq!(results["BTC/USD"].cancelled_count(), 1);
    let snap = mgr
        .get_book("BTC/USD")
        .expect("book")
        .create_snapshot(usize::MAX);
    assert!(snap.bids.is_empty());
}

#[test]
fn std_cancel_by_side_across_books() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("BTC/USD").expect("add book");

    let book = mgr.get_book("BTC/USD").expect("book must exist");
    let _ = book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new_uuid(), 110, 5, Side::Sell, TimeInForce::Gtc, None);

    let results = mgr.cancel_by_side_across_books(Side::Buy);
    assert_eq!(results["BTC/USD"].cancelled_count(), 1);
    let snap = mgr
        .get_book("BTC/USD")
        .expect("book")
        .create_snapshot(usize::MAX);
    assert!(snap.bids.is_empty());
    assert_eq!(snap.asks.len(), 1);
}

// ─── BookManagerTokio ───────────────────────────────────────────────────────

#[test]
fn tokio_default_creates_empty_manager() {
    let mgr: BookManagerTokio<()> = BookManagerTokio::default();
    assert_eq!(mgr.book_count(), 0);
    assert!(mgr.symbols().is_empty());
}

#[test]
fn tokio_add_and_get_book() {
    let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
    mgr.add_book("BTC/USD").expect("add book");
    assert!(mgr.has_book("BTC/USD"));
    assert_eq!(mgr.book_count(), 1);
}

#[test]
fn tokio_get_book_returns_none_for_unknown() {
    let mgr: BookManagerTokio<()> = BookManagerTokio::new();
    assert!(mgr.get_book("UNKNOWN").is_none());
}

#[test]
fn tokio_get_book_mut_returns_none_for_unknown() {
    let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
    assert!(mgr.get_book_mut("UNKNOWN").is_none());
}

#[test]
fn tokio_remove_book_returns_some_when_exists() {
    let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
    mgr.add_book("BTC/USD").expect("add book");
    assert!(mgr.remove_book("BTC/USD").is_some());
    assert_eq!(mgr.book_count(), 0);
}

#[test]
fn tokio_remove_book_returns_none_when_missing() {
    let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
    assert!(mgr.remove_book("MISSING").is_none());
}

#[test]
fn tokio_symbols_returns_all_books() {
    let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
    mgr.add_book("BTC/USD").expect("add book");
    mgr.add_book("ETH/USD").expect("add book");
    let mut symbols = mgr.symbols();
    symbols.sort();
    assert_eq!(symbols, vec!["BTC/USD", "ETH/USD"]);
}

#[test]
fn tokio_cancel_all_across_books() {
    let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
    mgr.add_book("BTC/USD").expect("add book");
    let book = mgr.get_book("BTC/USD").expect("book must exist");
    let _ = book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);

    let results = mgr.cancel_all_across_books();
    assert_eq!(results["BTC/USD"].cancelled_count(), 1);
    let snap = mgr
        .get_book("BTC/USD")
        .expect("book")
        .create_snapshot(usize::MAX);
    assert!(snap.bids.is_empty());
}

#[test]
fn tokio_cancel_by_user_across_books() {
    let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
    mgr.add_book("BTC/USD").expect("add book");
    let book = mgr.get_book("BTC/USD").expect("book must exist");
    let _ = book.add_limit_order_with_user(
        Id::new_uuid(),
        100,
        10,
        Side::Buy,
        TimeInForce::Gtc,
        Hash32::from([99u8; 32]),
        None,
    );

    let results = mgr.cancel_by_user_across_books(Hash32::from([99u8; 32]));
    assert_eq!(results["BTC/USD"].cancelled_count(), 1);
    let snap = mgr
        .get_book("BTC/USD")
        .expect("book")
        .create_snapshot(usize::MAX);
    assert!(snap.bids.is_empty());
}

#[test]
fn tokio_cancel_by_side_across_books() {
    let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
    mgr.add_book("BTC/USD").expect("add book");
    let book = mgr.get_book("BTC/USD").expect("book must exist");
    let _ = book.add_limit_order(Id::new_uuid(), 100, 10, Side::Sell, TimeInForce::Gtc, None);

    let results = mgr.cancel_by_side_across_books(Side::Sell);
    assert_eq!(results["BTC/USD"].cancelled_count(), 1);
    let snap = mgr
        .get_book("BTC/USD")
        .expect("book")
        .create_snapshot(usize::MAX);
    assert!(snap.asks.is_empty());
}

// ─── add_book duplicate-symbol rejection (#105) ─────────────────────────────

#[test]
fn std_add_book_duplicate_symbol_is_rejected_and_preserves_existing_issue_105() {
    use orderbook_rs::ManagerError;

    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("BTC/USD").expect("first add");
    // Seed a resting order into the existing book.
    if let Some(book) = mgr.get_book("BTC/USD") {
        let _ = book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    }

    // A second add_book for the same symbol must NOT overwrite the live book.
    match mgr.add_book("BTC/USD") {
        Err(ManagerError::BookAlreadyExists { symbol }) => assert_eq!(symbol, "BTC/USD"),
        other => panic!("expected BookAlreadyExists, got {other:?}"),
    }
    // The original book (and its resting order) survives unchanged.
    assert_eq!(mgr.book_count(), 1);
    let book = mgr.get_book("BTC/USD").expect("book still present");
    assert_eq!(
        book.best_bid(),
        Some(100),
        "existing resting order preserved"
    );
}

#[test]
fn tokio_add_book_duplicate_symbol_is_rejected_and_preserves_existing_issue_105() {
    use orderbook_rs::ManagerError;

    let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
    mgr.add_book("ETH/USD").expect("first add");
    if let Some(book) = mgr.get_book("ETH/USD") {
        let _ = book.add_limit_order(Id::new_uuid(), 200, 5, Side::Sell, TimeInForce::Gtc, None);
    }

    match mgr.add_book("ETH/USD") {
        Err(ManagerError::BookAlreadyExists { symbol }) => assert_eq!(symbol, "ETH/USD"),
        other => panic!("expected BookAlreadyExists, got {other:?}"),
    }
    assert_eq!(mgr.book_count(), 1);
    let book = mgr.get_book("ETH/USD").expect("book still present");
    assert_eq!(
        book.best_ask(),
        Some(200),
        "existing resting order preserved"
    );
}
