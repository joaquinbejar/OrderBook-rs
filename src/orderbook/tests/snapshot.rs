#[cfg(test)]
mod tests {
    use crate::OrderBookSnapshot;

    // Helper function to create an empty snapshot for testing
    fn create_empty_snapshot() -> OrderBookSnapshot {
        OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 12345678,
            bids: Vec::new(),
            asks: Vec::new(),
        }
    }

    // Helper function to create a snapshot with sample data
    fn create_sample_snapshot() -> OrderBookSnapshot {
        // Create bid levels
        let bid1 = crate::orderbook::tests::test_helpers::make_snapshot(1000, 10, 5, 2);

        let bid2 = crate::orderbook::tests::test_helpers::make_snapshot(990, 20, 0, 1);

        // Create ask levels
        let ask1 = crate::orderbook::tests::test_helpers::make_snapshot(1010, 15, 0, 3);

        let ask2 = crate::orderbook::tests::test_helpers::make_snapshot(1020, 25, 10, 2);

        OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 12345678,
            bids: vec![bid1, bid2],
            asks: vec![ask1, ask2],
        }
    }

    #[test]
    fn test_empty_snapshot_best_bid_ask() {
        let snapshot = create_empty_snapshot();

        assert_eq!(
            snapshot.best_bid(),
            None,
            "Empty book should have no best bid"
        );
        assert_eq!(
            snapshot.best_ask(),
            None,
            "Empty book should have no best ask"
        );
    }

    #[test]
    fn test_best_bid_ask() {
        let snapshot = create_sample_snapshot();

        // Best bid should be the highest bid price (1000) and its quantity
        assert_eq!(
            snapshot.best_bid(),
            Some((1000, 10)),
            "Best bid should be the highest price bid"
        );

        // Best ask should be the lowest ask price (1010) and its quantity
        assert_eq!(
            snapshot.best_ask(),
            Some((1010, 15)),
            "Best ask should be the lowest price ask"
        );
    }

    #[test]
    fn test_mid_price() {
        let snapshot = create_sample_snapshot();

        // Mid price is average of best bid and best ask
        let expected_mid_price = (1000.0 + 1010.0) / 2.0;
        assert_eq!(
            snapshot.mid_price(),
            Some(expected_mid_price),
            "Mid price should be average of best bid and ask"
        );

        // Empty book should have no mid price
        let empty_snapshot = create_empty_snapshot();
        assert_eq!(
            empty_snapshot.mid_price(),
            None,
            "Empty book should have no mid price"
        );
    }

    #[test]
    fn test_spread() {
        let snapshot = create_sample_snapshot();

        // Spread is best ask - best bid
        let expected_spread = 1010 - 1000;
        assert_eq!(
            snapshot.spread(),
            Some(expected_spread),
            "Spread should be best ask minus best bid"
        );

        // Empty book should have no spread
        let empty_snapshot = create_empty_snapshot();
        assert_eq!(
            empty_snapshot.spread(),
            None,
            "Empty book should have no spread"
        );
    }

    #[test]
    fn test_total_bid_volume() {
        let snapshot = create_sample_snapshot();

        // Total bid volume should include visible and hidden quantities
        let expected_volume = (10 + 5) + 20; // First bid + Second bid (visible + hidden)
        assert_eq!(
            snapshot.total_bid_volume(),
            expected_volume,
            "Total bid volume should sum all bid quantities"
        );

        // Empty book should have zero volume
        let empty_snapshot = create_empty_snapshot();
        assert_eq!(
            empty_snapshot.total_bid_volume(),
            0,
            "Empty book should have zero bid volume"
        );
    }

    #[test]
    fn test_total_ask_volume() {
        let snapshot = create_sample_snapshot();

        // Total ask volume should include visible and hidden quantities
        let expected_volume = 15 + (25 + 10); // First ask + Second ask (visible + hidden)
        assert_eq!(
            snapshot.total_ask_volume(),
            expected_volume,
            "Total ask volume should sum all ask quantities"
        );

        // Empty book should have zero volume
        let empty_snapshot = create_empty_snapshot();
        assert_eq!(
            empty_snapshot.total_ask_volume(),
            0,
            "Empty book should have zero ask volume"
        );
    }

    #[test]
    fn test_total_bid_value() {
        let snapshot = create_sample_snapshot();

        // Total bid value should be sum of price * total_quantity for each level
        let expected_value = 1000 * (10 + 5) + 990 * 20;
        assert_eq!(
            snapshot.total_bid_value(),
            expected_value,
            "Total bid value should sum price*quantity for all bids"
        );

        // Empty book should have zero value
        let empty_snapshot = create_empty_snapshot();
        assert_eq!(
            empty_snapshot.total_bid_value(),
            0,
            "Empty book should have zero bid value"
        );
    }

    #[test]
    fn test_total_ask_value() {
        let snapshot = create_sample_snapshot();

        // Total ask value should be sum of price * total_quantity for each level
        let expected_value = 1010 * 15 + 1020 * (25 + 10);
        assert_eq!(
            snapshot.total_ask_value(),
            expected_value,
            "Total ask value should sum price*quantity for all asks"
        );

        // Empty book should have zero value
        let empty_snapshot = create_empty_snapshot();
        assert_eq!(
            empty_snapshot.total_ask_value(),
            0,
            "Empty book should have zero ask value"
        );
    }

    #[test]
    fn test_snapshot_integrity() {
        let snapshot = create_sample_snapshot();

        // Check symbol and timestamp
        assert_eq!(snapshot.symbol, "TEST", "Symbol should match what was set");
        assert_eq!(
            snapshot.timestamp, 12345678,
            "Timestamp should match what was set"
        );

        // Check number of price levels
        assert_eq!(snapshot.bids.len(), 2, "Should have 2 bid levels");
        assert_eq!(snapshot.asks.len(), 2, "Should have 2 ask levels");

        // Check first bid properties
        assert_eq!(
            snapshot.bids[0].price(),
            1000,
            "First bid price should be 1000"
        );
        assert_eq!(
            snapshot.bids[0].visible_quantity(),
            10,
            "First bid visible quantity should be 10"
        );
        assert_eq!(
            snapshot.bids[0].hidden_quantity(),
            5,
            "First bid hidden quantity should be 5"
        );
        assert_eq!(
            snapshot.bids[0].order_count(),
            2,
            "First bid should have 2 orders"
        );

        // Check first ask properties
        assert_eq!(
            snapshot.asks[0].price(),
            1010,
            "First ask price should be 1010"
        );
        assert_eq!(
            snapshot.asks[0].visible_quantity(),
            15,
            "First ask visible quantity should be 15"
        );
        assert_eq!(
            snapshot.asks[0].hidden_quantity(),
            0,
            "First ask hidden quantity should be 0"
        );
        assert_eq!(
            snapshot.asks[0].order_count(),
            3,
            "First ask should have 3 orders"
        );
    }

    #[test]
    fn test_bid_ask_with_prices_out_of_order() {
        // Create snapshot with bid prices in ascending order (incorrect order)
        let bid1 = crate::orderbook::tests::test_helpers::make_snapshot(990, 20, 0, 1);

        let bid2 = crate::orderbook::tests::test_helpers::make_snapshot(1000, 10, 5, 2);

        let snapshot = OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 12345678,
            bids: vec![bid1, bid2],
            asks: Vec::new(),
        };

        // Best bid should still be the highest price (1000), even though it's not first in array
        assert_eq!(
            snapshot.best_bid(),
            Some((1000, 10)),
            "Best bid should be highest price regardless of array order"
        );
    }

    #[test]
    fn test_serialization_deserialization() {
        let original = create_sample_snapshot();

        // Serialize to JSON
        let serialized = serde_json::to_string(&original).expect("Failed to serialize");

        // Deserialize back to struct
        let deserialized: OrderBookSnapshot =
            serde_json::from_str(&serialized).expect("Failed to deserialize");

        // Verify all properties match
        assert_eq!(
            deserialized.symbol, original.symbol,
            "Symbol should match after serialization"
        );
        assert_eq!(
            deserialized.timestamp, original.timestamp,
            "Timestamp should match after serialization"
        );
        assert_eq!(
            deserialized.bids.len(),
            original.bids.len(),
            "Bid count should match after serialization"
        );
        assert_eq!(
            deserialized.asks.len(),
            original.asks.len(),
            "Ask count should match after serialization"
        );

        // Check first bid details
        assert_eq!(
            deserialized.bids[0].price(),
            original.bids[0].price(),
            "Bid price should match after serialization"
        );
        assert_eq!(
            deserialized.bids[0].visible_quantity(),
            original.bids[0].visible_quantity(),
            "Bid visible quantity should match after serialization"
        );

        // Check first ask details
        assert_eq!(
            deserialized.asks[0].price(),
            original.asks[0].price(),
            "Ask price should match after serialization"
        );
        assert_eq!(
            deserialized.asks[0].visible_quantity(),
            original.asks[0].visible_quantity(),
            "Ask visible quantity should match after serialization"
        );
    }
}

#[cfg(test)]
mod tests_bis {
    use crate::OrderBookSnapshot;

    // Helper function to create an improved implementation of best_bid
    fn find_best_bid(snapshot: &OrderBookSnapshot) -> Option<(u128, u64)> {
        snapshot
            .bids
            .iter()
            .map(|level| (level.price(), level.visible_quantity()))
            .max_by_key(|&(price, _)| price)
    }

    // Helper function to create an improved implementation of best_ask
    fn find_best_ask(snapshot: &OrderBookSnapshot) -> Option<(u128, u64)> {
        snapshot
            .asks
            .iter()
            .map(|level| (level.price(), level.visible_quantity()))
            .min_by_key(|&(price, _)| price)
    }

    // Create a snapshot with levels in random order
    fn create_unordered_snapshot() -> OrderBookSnapshot {
        // Create bid levels (out of order)
        let bid1 = crate::orderbook::tests::test_helpers::make_snapshot(980, 30, 0, 3);

        let bid2 = crate::orderbook::tests::test_helpers::make_snapshot(1000, 10, 5, 2);

        let bid3 = crate::orderbook::tests::test_helpers::make_snapshot(990, 20, 0, 1);

        // Create ask levels (out of order)
        let ask1 = crate::orderbook::tests::test_helpers::make_snapshot(1020, 25, 10, 2);

        let ask2 = crate::orderbook::tests::test_helpers::make_snapshot(1030, 35, 0, 4);

        let ask3 = crate::orderbook::tests::test_helpers::make_snapshot(1010, 15, 0, 3);

        OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 12345678,
            bids: vec![bid1, bid3, bid2], // Deliberately unordered
            asks: vec![ask2, ask1, ask3], // Deliberately unordered
        }
    }

    #[test]
    fn test_improved_best_bid_ask() {
        let snapshot = create_unordered_snapshot();

        // Find best bid and ask
        let best_bid = find_best_bid(&snapshot);
        let best_ask = find_best_ask(&snapshot);

        // Verify highest bid price
        assert_eq!(
            best_bid,
            Some((1000, 10)),
            "Best bid should be the highest price"
        );

        // Verify lowest ask price
        assert_eq!(
            best_ask,
            Some((1010, 15)),
            "Best ask should be the lowest price"
        );
    }

    #[test]
    fn test_mid_price_with_improved_methods() {
        let snapshot = create_unordered_snapshot();

        // Calculate mid price from best bid and ask
        let best_bid = find_best_bid(&snapshot);
        let best_ask = find_best_ask(&snapshot);

        let mid_price = match (best_bid, best_ask) {
            (Some((bid_price, _)), Some((ask_price, _))) => {
                Some((bid_price as f64 + ask_price as f64) / 2.0)
            }
            _ => None,
        };

        // Verify mid price
        assert_eq!(
            mid_price,
            Some(1005.0),
            "Mid price should be average of best bid and best ask"
        );
    }

    #[test]
    fn test_spread_with_improved_methods() {
        let snapshot = create_unordered_snapshot();

        // Calculate spread from best bid and ask
        let best_bid = find_best_bid(&snapshot);
        let best_ask = find_best_ask(&snapshot);

        let spread = match (best_bid, best_ask) {
            (Some((bid_price, _)), Some((ask_price, _))) => {
                Some(ask_price.saturating_sub(bid_price))
            }
            _ => None,
        };

        // Verify spread
        assert_eq!(spread, Some(10), "Spread should be ask price - bid price");
    }

    #[test]
    fn test_integration_with_sort() {
        let mut snapshot = create_unordered_snapshot();

        // Sort the bids by price in descending order
        snapshot.bids.sort_by_key(|b| std::cmp::Reverse(b.price()));

        // Sort the asks by price in ascending order
        snapshot.asks.sort_by_key(|a| a.price());

        // Now the first element should be the best price
        let best_bid = snapshot
            .bids
            .first()
            .map(|level| (level.price(), level.visible_quantity()));
        let best_ask = snapshot
            .asks
            .first()
            .map(|level| (level.price(), level.visible_quantity()));

        // Verify that sorting gives the correct best prices
        assert_eq!(
            best_bid,
            Some((1000, 10)),
            "First bid after sorting should be highest price"
        );
        assert_eq!(
            best_ask,
            Some((1010, 15)),
            "First ask after sorting should be lowest price"
        );
    }

    #[test]
    fn test_proposal_for_impl_best_bid_ask() {
        // This test shows how you could implement best_bid() and best_ask() in OrderBookSnapshot

        // Implementation for best_bid()
        fn best_bid(snapshot: &OrderBookSnapshot) -> Option<(u128, u64)> {
            snapshot
                .bids
                .iter()
                .map(|level| (level.price(), level.visible_quantity()))
                .max_by_key(|&(price, _)| price)
        }

        // Implementation for best_ask()
        fn best_ask(snapshot: &OrderBookSnapshot) -> Option<(u128, u64)> {
            snapshot
                .asks
                .iter()
                .map(|level| (level.price(), level.visible_quantity()))
                .min_by_key(|&(price, _)| price)
        }

        let snapshot = create_unordered_snapshot();

        // Verify proposed implementations
        assert_eq!(
            best_bid(&snapshot),
            Some((1000, 10)),
            "Proposed best_bid works correctly"
        );
        assert_eq!(
            best_ask(&snapshot),
            Some((1010, 15)),
            "Proposed best_ask works correctly"
        );
    }
}

#[cfg(test)]
mod test_orderbook_snapshot {
    use crate::OrderBookSnapshot;

    #[test]
    fn test_snapshot_methods() {
        // Create a snapshot with bid levels
        let bid1 = crate::orderbook::tests::test_helpers::make_snapshot(1000, 10, 5, 2);

        let bid2 = crate::orderbook::tests::test_helpers::make_snapshot(990, 20, 0, 1);

        // Create ask levels
        let ask1 = crate::orderbook::tests::test_helpers::make_snapshot(1010, 15, 0, 3);

        let ask2 = crate::orderbook::tests::test_helpers::make_snapshot(1020, 25, 10, 2);

        let snapshot = OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 12345678,
            bids: vec![bid1, bid2],
            asks: vec![ask1, ask2],
        };

        // Test total_bid_volume
        assert_eq!(snapshot.total_bid_volume(), 35); // 10 + 5 + 20

        // Test total_ask_volume
        assert_eq!(snapshot.total_ask_volume(), 50); // 15 + 25 + 10

        // Test total_bid_value
        assert_eq!(snapshot.total_bid_value(), 1000 * 15 + 990 * 20);

        // Test total_ask_value
        assert_eq!(snapshot.total_ask_value(), 1010 * 15 + 1020 * 35);
    }
}

#[cfg(test)]
mod test_snapshot_remaining {
    use crate::OrderBookSnapshot;

    #[test]
    fn test_empty_snapshot_volume_methods() {
        let empty_snapshot = OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 12345678,
            bids: Vec::new(),
            asks: Vec::new(),
        };

        // Test volume methods on empty snapshot
        assert_eq!(empty_snapshot.total_bid_volume(), 0);
        assert_eq!(empty_snapshot.total_ask_volume(), 0);
        assert_eq!(empty_snapshot.total_bid_value(), 0);
        assert_eq!(empty_snapshot.total_ask_value(), 0);
    }

    #[test]
    fn test_snapshot_tracing() {
        // Create a snapshot with a bid level
        let bid = crate::orderbook::tests::test_helpers::make_snapshot(1000, 10, 5, 2);

        // Create an ask level
        let ask = crate::orderbook::tests::test_helpers::make_snapshot(1010, 15, 0, 3);

        let snapshot = OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 12345678,
            bids: vec![bid],
            asks: vec![ask],
        };

        // Test methods that involve tracing
        let best_bid = snapshot.best_bid();
        let best_ask = snapshot.best_ask();
        let mid_price = snapshot.mid_price();
        let spread = snapshot.spread();
        let total_bid_volume = snapshot.total_bid_volume();
        let total_ask_volume = snapshot.total_ask_volume();
        let total_bid_value = snapshot.total_bid_value();
        let total_ask_value = snapshot.total_ask_value();

        // Verify results
        assert_eq!(best_bid, Some((1000, 10)));
        assert_eq!(best_ask, Some((1010, 15)));
        assert_eq!(mid_price, Some(1005.0));
        assert_eq!(spread, Some(10));
        assert_eq!(total_bid_volume, 15);
        assert_eq!(total_ask_volume, 15);
        assert_eq!(total_bid_value, 15000);
        assert_eq!(total_ask_value, 15150);
    }
}

#[cfg(test)]
mod test_snapshot_specific {
    use crate::OrderBookSnapshot;

    use tracing::trace;

    #[test]
    fn test_snapshot_trace_output() {
        // Create a test snapshot
        let bid = crate::orderbook::tests::test_helpers::make_snapshot(1000, 10, 0, 1);

        let ask = crate::orderbook::tests::test_helpers::make_snapshot(1010, 15, 0, 1);

        let snapshot = OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 12345678,
            bids: vec![bid],
            asks: vec![ask],
        };

        // Call functions that have trace output
        trace!("About to test snapshot trace outputs");

        let best_bid = snapshot.best_bid();
        trace!("Best bid: {:?}", best_bid);

        let best_ask = snapshot.best_ask();
        trace!("Best ask: {:?}", best_ask);

        // Verify correct results
        assert_eq!(best_bid, Some((1000, 10)));
        assert_eq!(best_ask, Some((1010, 15)));
    }
}

#[cfg(test)]
mod test_snapshot_engine_seq {
    use crate::DefaultOrderBook;
    use crate::orderbook::error::OrderBookError;
    use crate::orderbook::{ORDERBOOK_SNAPSHOT_FORMAT_VERSION, OrderBookSnapshotPackage};

    /// Round-trip an engine_seq value through the snapshot package: the
    /// counter must be restored verbatim, and the next mint must resume
    /// from that value (i.e. return `engine_seq` then advance to
    /// `engine_seq + 1`).
    #[test]
    fn test_snapshot_package_round_trips_engine_seq() {
        let original = DefaultOrderBook::new("ESQ");

        // Advance the counter past zero. After 5 calls the counter sits at 5.
        for _ in 0..5 {
            let _ = original.next_engine_seq();
        }
        assert_eq!(original.engine_seq(), 5, "counter primed to 5");

        let package = original
            .create_snapshot_package(10)
            .expect("build snapshot package");
        assert_eq!(
            package.engine_seq, 5,
            "package captures engine_seq from the source book"
        );
        assert_eq!(
            package.version, ORDERBOOK_SNAPSHOT_FORMAT_VERSION,
            "package carries the current format version"
        );

        // Round-trip via JSON.
        let json = package.to_json().expect("serialize to json");
        let parsed = OrderBookSnapshotPackage::from_json(&json).expect("deserialize from json");
        assert_eq!(
            parsed.engine_seq, 5,
            "engine_seq survives JSON encode/decode"
        );

        // Restore into a fresh book; the counter must equal 5.
        let mut restored = DefaultOrderBook::new("ESQ");
        restored
            .restore_from_snapshot_package(parsed)
            .expect("restore from package");
        assert_eq!(
            restored.engine_seq(),
            5,
            "restored book resumes its counter at the snapshotted value"
        );

        // Next mint returns 5 (the resumed value) and advances to 6.
        let next = restored.next_engine_seq();
        assert_eq!(next, 5, "next_engine_seq returns the resumed value");
        assert_eq!(
            restored.engine_seq(),
            6,
            "engine_seq advances to 6 after the mint"
        );
    }

    /// `version: 1` payloads — the legacy format that lacked `engine_seq`
    /// — must be rejected by `validate()` with the existing
    /// `Unsupported snapshot version` error after the bump to v2. No
    /// special-casing.
    #[test]
    fn test_snapshot_package_v1_payload_rejected_by_validate() {
        let payload = serde_json::json!({
            "version": 1u32,
            "snapshot": {
                "symbol": "V1",
                "timestamp": 0u64,
                "bids": [],
                "asks": []
            },
            "checksum": "deadbeef",
            "fee_schedule": null,
            "stp_mode": "None",
            "tick_size": null,
            "lot_size": null,
            "min_order_size": null,
            "max_order_size": null
        })
        .to_string();

        let package =
            OrderBookSnapshotPackage::from_json(&payload).expect("deserialize v1 payload");
        assert_eq!(package.version, 1, "version field reflects the payload");
        assert_eq!(
            package.engine_seq, 0,
            "missing engine_seq field defaults to 0"
        );

        let err = package
            .validate()
            .expect_err("v1 payload must be rejected after the v2 bump");

        match err {
            OrderBookError::InvalidOperation { message } => {
                assert!(
                    message.contains("Unsupported snapshot version"),
                    "error message must mention the unsupported version, got: {message}"
                );
                assert!(
                    message.contains('1'),
                    "error message must mention version 1, got: {message}"
                );
                assert!(
                    message.contains('2'),
                    "error message must mention expected version 2, got: {message}"
                );
            }
            other => panic!("expected InvalidOperation, got {other:?}"),
        }
    }

    /// Pure serde round-trip: a package with a non-trivial `engine_seq`
    /// value survives JSON encoding and decoding intact.
    #[test]
    fn test_snapshot_package_engine_seq_field_serializes_and_deserializes() {
        let book = DefaultOrderBook::new("FLD");
        let mut package = book
            .create_snapshot_package(10)
            .expect("build snapshot package");
        package.engine_seq = 12_345;

        let json = package.to_json().expect("serialize package to json");
        let decoded =
            OrderBookSnapshotPackage::from_json(&json).expect("deserialize package from json");

        assert_eq!(
            decoded.engine_seq, 12_345,
            "engine_seq round-trips through JSON unchanged"
        );
    }

    /// A `version: 2` payload that omits the `engine_seq` field entirely
    /// (e.g. produced by a downstream consumer that constructed the
    /// package via the legacy `OrderBookSnapshotPackage::new` entry
    /// point) must still deserialize, falling back to `0` via
    /// `#[serde(default)]`.
    #[test]
    fn test_snapshot_package_v2_payload_without_engine_seq_field_uses_default() {
        let payload = serde_json::json!({
            "version": ORDERBOOK_SNAPSHOT_FORMAT_VERSION,
            "snapshot": {
                "symbol": "V2",
                "timestamp": 0u64,
                "bids": [],
                "asks": []
            },
            "checksum": "deadbeef",
            "fee_schedule": null,
            "stp_mode": "None",
            "tick_size": null,
            "lot_size": null,
            "min_order_size": null,
            "max_order_size": null
            // engine_seq deliberately omitted — must default to 0.
        })
        .to_string();

        let package = OrderBookSnapshotPackage::from_json(&payload)
            .expect("deserialize v2 payload missing engine_seq");
        assert_eq!(
            package.version, ORDERBOOK_SNAPSHOT_FORMAT_VERSION,
            "version field reflects the payload"
        );
        assert_eq!(
            package.engine_seq, 0,
            "omitted engine_seq must default to 0 via #[serde(default)]"
        );
    }
}
