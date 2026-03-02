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

        let mut restored = DefaultOrderBook::new("TEST");
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

        let mut restored = DefaultOrderBook::new("JSON");
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

        let mut restored = DefaultOrderBook::new("CHK");
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

        let mut restored = DefaultOrderBook::new("VER");
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

        let mut other = DefaultOrderBook::new("TWO");
        let err = other
            .restore_from_snapshot_package(package)
            .expect_err("restore should fail when symbol differs");

        assert!(matches!(err, OrderBookError::InvalidOperation { .. }));
    }

    // ── Config round-trip tests ─────────────────────────────────────────

    #[test]
    fn snapshot_package_preserves_fee_schedule() {
        use orderbook_rs::FeeSchedule;

        let mut original = DefaultOrderBook::new("FEE");
        populate_order_book(&original);
        original.set_fee_schedule(Some(FeeSchedule::new(-2, 5)));

        let package = original.create_snapshot_package(10).expect("snapshot");
        assert!(package.fee_schedule.is_some());

        let mut restored = DefaultOrderBook::new("FEE");
        restored
            .restore_from_snapshot_package(package)
            .expect("restore");

        assert_eq!(restored.fee_schedule(), original.fee_schedule());
    }

    #[test]
    fn snapshot_package_preserves_stp_mode() {
        use orderbook_rs::orderbook::stp::STPMode;

        let mut original = DefaultOrderBook::new("STP");
        populate_order_book(&original);
        original.set_stp_mode(STPMode::CancelTaker);

        let package = original.create_snapshot_package(10).expect("snapshot");
        assert_eq!(package.stp_mode, STPMode::CancelTaker);

        let mut restored = DefaultOrderBook::new("STP");
        restored
            .restore_from_snapshot_package(package)
            .expect("restore");

        assert_eq!(restored.stp_mode(), STPMode::CancelTaker);
    }

    #[test]
    fn snapshot_package_preserves_tick_size() {
        let mut original = DefaultOrderBook::new("TICK");
        populate_order_book(&original);
        original.set_tick_size(100);

        let package = original.create_snapshot_package(10).expect("snapshot");
        assert_eq!(package.tick_size, Some(100));

        let mut restored = DefaultOrderBook::new("TICK");
        restored
            .restore_from_snapshot_package(package)
            .expect("restore");

        assert_eq!(restored.tick_size(), Some(100));
    }

    #[test]
    fn snapshot_package_preserves_lot_size() {
        let mut original = DefaultOrderBook::new("LOT");
        populate_order_book(&original);
        original.set_lot_size(10);

        let package = original.create_snapshot_package(10).expect("snapshot");
        assert_eq!(package.lot_size, Some(10));

        let mut restored = DefaultOrderBook::new("LOT");
        restored
            .restore_from_snapshot_package(package)
            .expect("restore");

        assert_eq!(restored.lot_size(), Some(10));
    }

    #[test]
    fn snapshot_package_preserves_min_max_order_size() {
        let mut original = DefaultOrderBook::new("SIZE");
        populate_order_book(&original);
        original.set_min_order_size(1);
        original.set_max_order_size(1000);

        let package = original.create_snapshot_package(10).expect("snapshot");
        assert_eq!(package.min_order_size, Some(1));
        assert_eq!(package.max_order_size, Some(1000));

        let mut restored = DefaultOrderBook::new("SIZE");
        restored
            .restore_from_snapshot_package(package)
            .expect("restore");

        assert_eq!(restored.min_order_size(), Some(1));
        assert_eq!(restored.max_order_size(), Some(1000));
    }

    #[test]
    fn snapshot_package_preserves_all_config_fields() {
        use orderbook_rs::FeeSchedule;
        use orderbook_rs::orderbook::stp::STPMode;

        let mut original = DefaultOrderBook::new("ALL");
        populate_order_book(&original);
        original.set_fee_schedule(Some(FeeSchedule::new(-1, 3)));
        original.set_stp_mode(STPMode::CancelBoth);
        original.set_tick_size(50);
        original.set_lot_size(5);
        original.set_min_order_size(1);
        original.set_max_order_size(500);

        let package = original.create_snapshot_package(10).expect("snapshot");

        let mut restored = DefaultOrderBook::new("ALL");
        restored
            .restore_from_snapshot_package(package)
            .expect("restore");

        assert_eq!(restored.fee_schedule(), original.fee_schedule());
        assert_eq!(restored.stp_mode(), STPMode::CancelBoth);
        assert_eq!(restored.tick_size(), Some(50));
        assert_eq!(restored.lot_size(), Some(5));
        assert_eq!(restored.min_order_size(), Some(1));
        assert_eq!(restored.max_order_size(), Some(500));
    }

    #[test]
    fn snapshot_package_backward_compat_no_config_fields() {
        let book = DefaultOrderBook::new("OLD");
        populate_order_book(&book);

        // Simulate an old snapshot without config fields by serializing
        // and stripping the config keys from JSON.
        let package = book.create_snapshot_package(10).expect("snapshot");
        let json = package.to_json().expect("serialize");

        // Parse as generic JSON, remove config keys, re-serialize
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        if let Some(obj) = value.as_object_mut() {
            obj.remove("fee_schedule");
            obj.remove("stp_mode");
            obj.remove("tick_size");
            obj.remove("lot_size");
            obj.remove("min_order_size");
            obj.remove("max_order_size");
        }
        let stripped_json = serde_json::to_string(&value).expect("re-serialize");

        // Should deserialize with defaults
        let mut restored = DefaultOrderBook::new("OLD");
        restored
            .restore_from_snapshot_json(&stripped_json)
            .expect("old snapshot without config fields should still load");

        assert_eq!(restored.fee_schedule(), None);
        assert_eq!(
            restored.stp_mode(),
            orderbook_rs::orderbook::stp::STPMode::None
        );
        assert_eq!(restored.tick_size(), None);
        assert_eq!(restored.lot_size(), None);
        assert_eq!(restored.min_order_size(), None);
        assert_eq!(restored.max_order_size(), None);
    }

    #[test]
    fn snapshot_json_round_trip_preserves_config() {
        use orderbook_rs::FeeSchedule;
        use orderbook_rs::orderbook::stp::STPMode;

        let mut original = DefaultOrderBook::new("JCFG");
        populate_order_book(&original);
        original.set_fee_schedule(Some(FeeSchedule::new(0, 4)));
        original.set_stp_mode(STPMode::CancelMaker);
        original.set_tick_size(25);
        original.set_lot_size(2);
        original.set_min_order_size(1);
        original.set_max_order_size(999);

        let json = original.snapshot_to_json(10).expect("snapshot to json");

        let mut restored = DefaultOrderBook::new("JCFG");
        restored.restore_from_snapshot_json(&json).expect("restore");

        assert_eq!(restored.fee_schedule(), original.fee_schedule());
        assert_eq!(restored.stp_mode(), STPMode::CancelMaker);
        assert_eq!(restored.tick_size(), Some(25));
        assert_eq!(restored.lot_size(), Some(2));
        assert_eq!(restored.min_order_size(), Some(1));
        assert_eq!(restored.max_order_size(), Some(999));
    }
}
