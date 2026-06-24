//! Tests for cross-book mass cancel operations on BookManagerStd and BookManagerTokio.

#[cfg(test)]
mod tests_cross_book_cancel {
    use orderbook_rs::orderbook::manager::{BookManager, BookManagerStd, BookManagerTokio};
    use pricelevel::{Hash32, Id, Side, TimeInForce};

    // ═══════════════════════════════════════════════════════════════════════
    // BookManagerStd
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn std_cancel_all_across_books() {
        let mut mgr: BookManagerStd<()> = BookManagerStd::new();
        mgr.add_book("BTC/USD").expect("add book");
        mgr.add_book("ETH/USD").expect("add book");

        if let Some(book) = mgr.get_book("BTC/USD") {
            book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
                .expect("add bid");
            book.add_limit_order(Id::new_uuid(), 200, 5, Side::Sell, TimeInForce::Gtc, None)
                .expect("add ask");
        }
        if let Some(book) = mgr.get_book("ETH/USD") {
            book.add_limit_order(Id::new_uuid(), 50, 20, Side::Buy, TimeInForce::Gtc, None)
                .expect("add bid");
        }

        let results = mgr.cancel_all_across_books();

        assert_eq!(results.len(), 2);
        assert_eq!(results.get("BTC/USD").map(|r| r.cancelled_count()), Some(2));
        assert_eq!(results.get("ETH/USD").map(|r| r.cancelled_count()), Some(1));

        // Verify books are empty
        assert_eq!(mgr.get_book("BTC/USD").and_then(|b| b.best_bid()), None);
        assert_eq!(mgr.get_book("ETH/USD").and_then(|b| b.best_bid()), None);
    }

    #[test]
    fn std_cancel_all_across_empty_books() {
        let mut mgr: BookManagerStd<()> = BookManagerStd::new();
        mgr.add_book("BTC/USD").expect("add book");
        mgr.add_book("ETH/USD").expect("add book");

        let results = mgr.cancel_all_across_books();
        assert_eq!(results.len(), 2);
        for result in results.values() {
            assert_eq!(result.cancelled_count(), 0);
        }
    }

    #[test]
    fn std_cancel_all_no_books() {
        let mgr: BookManagerStd<()> = BookManagerStd::new();
        let results = mgr.cancel_all_across_books();
        assert!(results.is_empty());
    }

    #[test]
    fn std_cancel_by_user_across_books() {
        let mut mgr: BookManagerStd<()> = BookManagerStd::new();
        mgr.add_book("BTC/USD").expect("add book");
        mgr.add_book("ETH/USD").expect("add book");

        let user_a = Hash32::from([1u8; 32]);
        let user_b = Hash32::from([2u8; 32]);

        if let Some(book) = mgr.get_book("BTC/USD") {
            book.add_limit_order_with_user(
                Id::new_uuid(),
                100,
                10,
                Side::Buy,
                TimeInForce::Gtc,
                user_a,
                None,
            )
            .expect("add");
            book.add_limit_order_with_user(
                Id::new_uuid(),
                200,
                5,
                Side::Sell,
                TimeInForce::Gtc,
                user_b,
                None,
            )
            .expect("add");
        }
        if let Some(book) = mgr.get_book("ETH/USD") {
            book.add_limit_order_with_user(
                Id::new_uuid(),
                50,
                20,
                Side::Buy,
                TimeInForce::Gtc,
                user_a,
                None,
            )
            .expect("add");
        }

        let results = mgr.cancel_by_user_across_books(user_a);

        assert_eq!(results.len(), 2);
        assert_eq!(results.get("BTC/USD").map(|r| r.cancelled_count()), Some(1));
        assert_eq!(results.get("ETH/USD").map(|r| r.cancelled_count()), Some(1));

        // user_b's order should still exist
        assert!(mgr.get_book("BTC/USD").and_then(|b| b.best_ask()).is_some());
    }

    #[test]
    fn std_cancel_by_side_across_books() {
        let mut mgr: BookManagerStd<()> = BookManagerStd::new();
        mgr.add_book("BTC/USD").expect("add book");
        mgr.add_book("ETH/USD").expect("add book");

        if let Some(book) = mgr.get_book("BTC/USD") {
            book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
                .expect("add");
            book.add_limit_order(Id::new_uuid(), 200, 5, Side::Sell, TimeInForce::Gtc, None)
                .expect("add");
        }
        if let Some(book) = mgr.get_book("ETH/USD") {
            book.add_limit_order(Id::new_uuid(), 50, 20, Side::Buy, TimeInForce::Gtc, None)
                .expect("add");
            book.add_limit_order(Id::new_uuid(), 60, 15, Side::Sell, TimeInForce::Gtc, None)
                .expect("add");
        }

        let results = mgr.cancel_by_side_across_books(Side::Buy);

        assert_eq!(results.len(), 2);
        assert_eq!(results.get("BTC/USD").map(|r| r.cancelled_count()), Some(1));
        assert_eq!(results.get("ETH/USD").map(|r| r.cancelled_count()), Some(1));

        // Sell sides should still have orders
        assert!(mgr.get_book("BTC/USD").and_then(|b| b.best_ask()).is_some());
        assert!(mgr.get_book("ETH/USD").and_then(|b| b.best_ask()).is_some());

        // Buy sides should be empty
        assert_eq!(mgr.get_book("BTC/USD").and_then(|b| b.best_bid()), None);
        assert_eq!(mgr.get_book("ETH/USD").and_then(|b| b.best_bid()), None);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // BookManagerTokio
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn tokio_cancel_all_across_books() {
        let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
        mgr.add_book("BTC/USD").expect("add book");
        mgr.add_book("ETH/USD").expect("add book");

        if let Some(book) = mgr.get_book("BTC/USD") {
            book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
                .expect("add");
        }
        if let Some(book) = mgr.get_book("ETH/USD") {
            book.add_limit_order(Id::new_uuid(), 50, 20, Side::Sell, TimeInForce::Gtc, None)
                .expect("add");
        }

        let results = mgr.cancel_all_across_books();

        assert_eq!(results.len(), 2);
        let total: usize = results.values().map(|r| r.cancelled_count()).sum();
        assert_eq!(total, 2);
    }

    #[test]
    fn tokio_cancel_by_user_across_books() {
        let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
        mgr.add_book("BTC/USD").expect("add book");

        let user = Hash32::from([7u8; 32]);

        if let Some(book) = mgr.get_book("BTC/USD") {
            book.add_limit_order_with_user(
                Id::new_uuid(),
                100,
                10,
                Side::Buy,
                TimeInForce::Gtc,
                user,
                None,
            )
            .expect("add");
        }

        let results = mgr.cancel_by_user_across_books(user);
        assert_eq!(results.get("BTC/USD").map(|r| r.cancelled_count()), Some(1));
    }

    #[test]
    fn tokio_cancel_by_side_across_books() {
        let mut mgr: BookManagerTokio<()> = BookManagerTokio::new();
        mgr.add_book("BTC/USD").expect("add book");

        if let Some(book) = mgr.get_book("BTC/USD") {
            book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
                .expect("add");
            book.add_limit_order(Id::new_uuid(), 200, 5, Side::Sell, TimeInForce::Gtc, None)
                .expect("add");
        }

        let results = mgr.cancel_by_side_across_books(Side::Sell);
        assert_eq!(results.get("BTC/USD").map(|r| r.cancelled_count()), Some(1));

        // Buy side should remain
        assert!(mgr.get_book("BTC/USD").and_then(|b| b.best_bid()).is_some());
    }
}
