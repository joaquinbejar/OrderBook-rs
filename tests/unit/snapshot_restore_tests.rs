#[cfg(test)]
mod tests_snapshot_restore {
    use orderbook_rs::orderbook::ORDERBOOK_SNAPSHOT_FORMAT_VERSION;
    use orderbook_rs::{DefaultOrderBook, OrderBook, OrderBookError};
    use pricelevel::{Id, Side, TimeInForce};

    fn populate_order_book(book: &OrderBook<()>) -> Vec<Id> {
        let first = Id::from_u64(1);
        let second = Id::from_u64(2);
        let third = Id::from_u64(3);
        let fourth = Id::from_u64(4);

        book.add_limit_order(first, 10_000, 5, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");
        book.add_limit_order(second, 9_900, 7, Side::Buy, TimeInForce::Gtc, None)
            .expect("add bid");
        book.add_limit_order(third, 10_100, 4, Side::Sell, TimeInForce::Gtc, None)
            .expect("add ask");
        book.add_limit_order(fourth, 10_200, 6, Side::Sell, TimeInForce::Gtc, None)
            .expect("add ask");

        vec![first, second, third, fourth]
    }

    #[test]
    fn snapshot_package_round_trip_restores_orders() {
        let original = DefaultOrderBook::new("TEST");
        let order_ids = populate_order_book(&original);

        let package = original
            .create_snapshot_package(10)
            .expect("snapshot package");

        let restored = DefaultOrderBook::new("TEST");
        restored
            .restore_from_snapshot_package(package)
            .expect("restore from package");

        assert_eq!(restored.best_bid(), Some(10_000));
        assert_eq!(restored.best_ask(), Some(10_100));

        for order_id in order_ids {
            let restored_order = restored
                .get_order(order_id)
                .expect("order should be restored");
            assert_eq!(restored_order.id(), order_id);
        }
    }

    #[test]
    fn snapshot_json_round_trip_restores_book_state() {
        let original = DefaultOrderBook::new("JSON");
        populate_order_book(&original);

        let json_payload = original
            .snapshot_to_json(10)
            .expect("serialize snapshot to json");

        let restored = DefaultOrderBook::new("JSON");
        restored
            .restore_from_snapshot_json(&json_payload)
            .expect("restore from json");

        assert_eq!(restored.best_bid(), Some(10_000));
        assert_eq!(restored.best_ask(), Some(10_100));
        assert_eq!(restored.mid_price(), Some(10_050.0));
    }

    #[test]
    fn restore_rejects_checksum_mismatch() {
        let book = DefaultOrderBook::new("CHK");
        populate_order_book(&book);

        let mut tampered = book.create_snapshot_package(10).expect("snapshot package");
        tampered.checksum = "deadbeef".to_string();

        let restored = DefaultOrderBook::new("CHK");
        let err = restored
            .restore_from_snapshot_package(tampered)
            .expect_err("checksum mismatch should be detected");

        assert!(matches!(err, OrderBookError::ChecksumMismatch { .. }));
    }

    #[test]
    fn restore_rejects_version_mismatch() {
        let book = DefaultOrderBook::new("VER");
        populate_order_book(&book);

        let mut package = book.create_snapshot_package(10).expect("snapshot package");
        package.version = ORDERBOOK_SNAPSHOT_FORMAT_VERSION + 1;

        let restored = DefaultOrderBook::new("VER");
        let err = restored
            .restore_from_snapshot_package(package)
            .expect_err("version mismatch should be rejected");

        assert!(matches!(err, OrderBookError::InvalidOperation { .. }));
    }

    #[test]
    fn restore_rejects_symbol_mismatch() {
        let book = DefaultOrderBook::new("ONE");
        populate_order_book(&book);

        let package = book.create_snapshot_package(10).expect("snapshot package");

        let other = DefaultOrderBook::new("TWO");
        let err = other
            .restore_from_snapshot_package(package)
            .expect_err("restore should fail when symbol differs");

        assert!(matches!(err, OrderBookError::InvalidOperation { .. }));
    }
}
