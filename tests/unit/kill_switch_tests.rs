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
}
