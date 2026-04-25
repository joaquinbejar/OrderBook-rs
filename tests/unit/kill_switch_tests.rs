//! Integration tests for the operational kill switch on `OrderBook<T>`.
//!
//! The kill switch halts new flow (`submit_*`, `add_order`, non-cancel
//! `update_order`) without dropping the book, while keeping cancel and
//! mass-cancel paths open so operators can drain the resting book.

#[cfg(test)]
mod tests_kill_switch {
    use orderbook_rs::orderbook::order_state::{OrderStateTracker, OrderStatus};
    use orderbook_rs::{OrderBook, OrderBookError};
    use pricelevel::{Hash32, Id, OrderUpdate, Price, Quantity, Side, TimeInForce};

    fn new_book() -> OrderBook<()> {
        OrderBook::new("TEST")
    }

    fn book_with_tracker() -> OrderBook<()> {
        let mut book = OrderBook::<()>::new("TEST");
        book.set_order_state_tracker(OrderStateTracker::new());
        book
    }

    // ───────────────────────────────────────────────────────────────────
    // Submit / add gates
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn submit_market_under_kill_switch_returns_kill_switch_active() {
        let book = new_book();
        // Seed liquidity so the only failure mode is the kill switch.
        book.add_limit_order(Id::new_uuid(), 100, 10, Side::Sell, TimeInForce::Gtc, None)
            .expect("seed resting ask");

        book.engage_kill_switch();

        let result = book.submit_market_order(Id::new_uuid(), 5, Side::Buy);
        assert!(
            matches!(result, Err(OrderBookError::KillSwitchActive)),
            "expected KillSwitchActive, got {result:?}"
        );
    }

    #[test]
    fn submit_market_with_user_under_kill_switch_returns_kill_switch_active() {
        let book = new_book();
        book.add_limit_order(Id::new_uuid(), 100, 10, Side::Sell, TimeInForce::Gtc, None)
            .expect("seed resting ask");

        book.engage_kill_switch();

        let result = book.submit_market_order_with_user(
            Id::new_uuid(),
            5,
            Side::Buy,
            Hash32::new([1u8; 32]),
        );
        assert!(
            matches!(result, Err(OrderBookError::KillSwitchActive)),
            "expected KillSwitchActive, got {result:?}"
        );
    }

    #[test]
    fn add_order_under_kill_switch_returns_kill_switch_active() {
        let book = new_book();
        book.engage_kill_switch();

        let result =
            book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        assert!(
            matches!(result, Err(OrderBookError::KillSwitchActive)),
            "expected KillSwitchActive, got {result:?}"
        );
        assert_eq!(book.best_bid(), None, "rejected order must not rest");
    }

    // ───────────────────────────────────────────────────────────────────
    // update_order — non-cancel variants are gated, Cancel passes
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn update_order_price_under_kill_switch_returns_kill_switch_active() {
        let book = new_book();
        let order_id = Id::new_uuid();
        book.add_limit_order(order_id, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed bid");

        book.engage_kill_switch();

        let result = book.update_order(OrderUpdate::UpdatePrice {
            order_id,
            new_price: Price::new(101),
        });
        assert!(
            matches!(result, Err(OrderBookError::KillSwitchActive)),
            "expected KillSwitchActive, got {result:?}"
        );
        // Original order untouched.
        assert!(book.get_order(order_id).is_some());
    }

    #[test]
    fn update_order_quantity_under_kill_switch_returns_kill_switch_active() {
        let book = new_book();
        let order_id = Id::new_uuid();
        book.add_limit_order(order_id, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed bid");

        book.engage_kill_switch();

        let result = book.update_order(OrderUpdate::UpdateQuantity {
            order_id,
            new_quantity: Quantity::new(20),
        });
        assert!(
            matches!(result, Err(OrderBookError::KillSwitchActive)),
            "expected KillSwitchActive, got {result:?}"
        );
    }

    #[test]
    fn update_order_price_and_quantity_under_kill_switch_returns_kill_switch_active() {
        let book = new_book();
        let order_id = Id::new_uuid();
        book.add_limit_order(order_id, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed bid");

        book.engage_kill_switch();

        let result = book.update_order(OrderUpdate::UpdatePriceAndQuantity {
            order_id,
            new_price: Price::new(99),
            new_quantity: Quantity::new(7),
        });
        assert!(
            matches!(result, Err(OrderBookError::KillSwitchActive)),
            "expected KillSwitchActive, got {result:?}"
        );
    }

    #[test]
    fn update_order_replace_under_kill_switch_returns_kill_switch_active() {
        let book = new_book();
        let order_id = Id::new_uuid();
        book.add_limit_order(order_id, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed bid");

        book.engage_kill_switch();

        let result = book.update_order(OrderUpdate::Replace {
            order_id,
            price: Price::new(99),
            quantity: Quantity::new(7),
            side: Side::Buy,
        });
        assert!(
            matches!(result, Err(OrderBookError::KillSwitchActive)),
            "expected KillSwitchActive, got {result:?}"
        );
    }

    #[test]
    fn update_order_cancel_under_kill_switch_succeeds() {
        let book = new_book();
        let order_id = Id::new_uuid();
        book.add_limit_order(order_id, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed bid");

        book.engage_kill_switch();

        // Cancel must pass through — operators rely on this to drain.
        let result = book.update_order(OrderUpdate::Cancel { order_id });
        assert!(
            result.is_ok(),
            "Cancel under kill switch must succeed; got {result:?}"
        );
        assert!(
            book.get_order(order_id).is_none(),
            "order must be cancelled"
        );
    }

    // ───────────────────────────────────────────────────────────────────
    // Cancel paths are NOT gated
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn cancel_order_under_kill_switch_succeeds() {
        let book = new_book();
        let order_id = Id::new_uuid();
        book.add_limit_order(order_id, 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed bid");

        book.engage_kill_switch();

        let cancelled = book
            .cancel_order(order_id)
            .expect("cancel under kill switch");
        assert!(
            cancelled.is_some(),
            "cancel should return the cancelled order"
        );
        assert!(book.get_order(order_id).is_none());
    }

    #[test]
    fn mass_cancel_by_side_under_kill_switch_succeeds() {
        let book = new_book();
        for price in [99, 100, 101] {
            book.add_limit_order(Id::new_uuid(), price, 5, Side::Buy, TimeInForce::Gtc, None)
                .expect("seed bid");
        }
        book.add_limit_order(Id::new_uuid(), 110, 5, Side::Sell, TimeInForce::Gtc, None)
            .expect("seed ask");

        book.engage_kill_switch();

        let result = book.cancel_orders_by_side(Side::Buy);
        assert_eq!(result.cancelled_count(), 3, "all bids must be cancelled");
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), Some(110), "ask side untouched");
    }

    #[test]
    fn cancel_all_under_kill_switch_succeeds() {
        let book = new_book();
        for price in [99, 100] {
            book.add_limit_order(Id::new_uuid(), price, 5, Side::Buy, TimeInForce::Gtc, None)
                .expect("seed bid");
        }
        for price in [110, 111] {
            book.add_limit_order(Id::new_uuid(), price, 5, Side::Sell, TimeInForce::Gtc, None)
                .expect("seed ask");
        }

        book.engage_kill_switch();

        let result = book.cancel_all_orders();
        assert_eq!(result.cancelled_count(), 4);
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), None);
    }

    // ───────────────────────────────────────────────────────────────────
    // Release resumes new flow; tracker integration
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn release_then_submit_succeeds() {
        let book = new_book();
        book.add_limit_order(Id::new_uuid(), 100, 10, Side::Sell, TimeInForce::Gtc, None)
            .expect("seed resting ask");

        book.engage_kill_switch();
        let blocked = book.submit_market_order(Id::new_uuid(), 5, Side::Buy);
        assert!(matches!(blocked, Err(OrderBookError::KillSwitchActive)));

        book.release_kill_switch();
        let resumed = book.submit_market_order(Id::new_uuid(), 5, Side::Buy);
        assert!(
            resumed.is_ok(),
            "submit must succeed after release; got {resumed:?}"
        );
    }

    #[test]
    fn tracker_records_rejected_on_kill_switch() {
        let book = book_with_tracker();
        book.engage_kill_switch();

        let order_id = Id::new_uuid();
        let result = book.add_limit_order(order_id, 100, 10, Side::Buy, TimeInForce::Gtc, None);
        assert!(matches!(result, Err(OrderBookError::KillSwitchActive)));

        let status = book.order_status(order_id);
        match status {
            Some(OrderStatus::Rejected { reason }) => {
                assert_eq!(reason, "kill switch active");
            }
            other => panic!("expected Rejected with kill switch reason, got {other:?}"),
        }
    }

    // ───────────────────────────────────────────────────────────────────
    // Snapshot round-trip
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn kill_switch_state_round_trips_through_snapshot() {
        let original = OrderBook::<()>::new("TEST");
        original
            .add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed bid");
        original.engage_kill_switch();
        assert!(original.is_kill_switch_engaged());

        // Serialize to JSON, deserialize, restore — the canonical disaster
        // recovery path.
        let json = original
            .snapshot_to_json(10)
            .expect("serialize snapshot to json");

        let mut restored = OrderBook::<()>::new("TEST");
        restored
            .restore_from_snapshot_json(&json)
            .expect("restore from json");

        assert!(
            restored.is_kill_switch_engaged(),
            "kill switch state must round-trip through snapshot/restore"
        );

        // And: the restored book actually enforces the gate.
        let result =
            restored.add_limit_order(Id::new_uuid(), 99, 5, Side::Buy, TimeInForce::Gtc, None);
        assert!(
            matches!(result, Err(OrderBookError::KillSwitchActive)),
            "restored book with engaged kill switch must reject new flow; got {result:?}"
        );
    }

    #[test]
    fn legacy_v2_snapshot_without_kill_switch_field_defaults_to_false() {
        // Build a fresh package and drop the `kill_switch_engaged` key
        // from the JSON to simulate a v2 payload written before this
        // field existed. `#[serde(default)]` should fill it back in
        // with `false` on deserialization.
        let book = OrderBook::<()>::new("TEST");
        book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed bid");

        let json = book.snapshot_to_json(10).expect("serialize");
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        let obj = value.as_object_mut().expect("top level object");
        obj.remove("kill_switch_engaged");
        let stripped = serde_json::to_string(&value).expect("re-serialize");
        assert!(
            !stripped.contains("kill_switch_engaged"),
            "preflight: stripped JSON must not carry the new field"
        );

        let mut restored = OrderBook::<()>::new("TEST");
        restored
            .restore_from_snapshot_json(&stripped)
            .expect("restore stripped json");

        assert!(
            !restored.is_kill_switch_engaged(),
            "missing field must default to false"
        );
    }
}
