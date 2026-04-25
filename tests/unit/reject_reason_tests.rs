//! Integration tests for the closed `RejectReason` taxonomy.
//!
//! Issue #55 — `OrderStatus::Rejected.reason: RejectReason`. Confirms
//! that every reject path that already transitions the tracker emits
//! the typed enum, and that the new risk-gate hook records the
//! correct variant before propagating the typed error.

#[cfg(test)]
mod tests_reject_reason {
    use orderbook_rs::orderbook::order_state::{OrderStateTracker, OrderStatus};
    use orderbook_rs::{OrderBook, OrderBookError, ReferencePriceSource, RejectReason, RiskConfig};
    use pricelevel::{Hash32, Id, Side, TimeInForce};

    fn book_with_tracker() -> OrderBook<()> {
        let mut book = OrderBook::<()>::new("TEST");
        book.set_order_state_tracker(OrderStateTracker::new());
        book
    }

    fn account(byte: u8) -> Hash32 {
        Hash32::new([byte; 32])
    }

    /// Seed two crossing orders so a trade prints and `last_trade_price`
    /// is set. After the helper returns, the book has no resting orders.
    fn seed_last_trade_price(book: &OrderBook<()>, price: u128) {
        book.add_limit_order(
            Id::new_uuid(),
            price,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("seed resting ask");
        book.add_limit_order(Id::new_uuid(), price, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("aggressive buy fills the ask");
    }

    // ───────────────────────────────────────────────────────────────────
    // Kill switch
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn kill_switch_reject_records_kill_switch_active_in_tracker() {
        let book = book_with_tracker();
        book.engage_kill_switch();

        let order_id = Id::new_uuid();
        let result = book.add_limit_order(order_id, 100, 10, Side::Buy, TimeInForce::Gtc, None);
        assert!(matches!(result, Err(OrderBookError::KillSwitchActive)));

        let status = book.order_status(order_id);
        assert_eq!(
            status,
            Some(OrderStatus::Rejected {
                reason: RejectReason::KillSwitchActive,
            }),
            "expected Rejected{{KillSwitchActive}}, got {status:?}"
        );
    }

    // ───────────────────────────────────────────────────────────────────
    // Risk gates
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn risk_max_open_reject_records_risk_max_open_orders_in_tracker() {
        let mut book = book_with_tracker();
        book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(1));
        let acct = account(11);

        // Fill the only open-order slot.
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        )
        .expect("first admission");

        // Second attempt is rejected on the open-orders gate.
        let rejected_id = Id::new_uuid();
        let result = book.add_limit_order_with_user(
            rejected_id,
            100,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        );
        assert!(
            matches!(result, Err(OrderBookError::RiskMaxOpenOrders { .. })),
            "expected RiskMaxOpenOrders, got {result:?}"
        );

        let status = book.order_status(rejected_id);
        assert_eq!(
            status,
            Some(OrderStatus::Rejected {
                reason: RejectReason::RiskMaxOpenOrders,
            }),
            "expected Rejected{{RiskMaxOpenOrders}}, got {status:?}"
        );
    }

    #[test]
    fn risk_max_notional_reject_records_risk_max_notional_in_tracker() {
        let mut book = book_with_tracker();
        // 1_000 notional ceiling per account.
        book.set_risk_config(RiskConfig::new().with_max_notional_per_account(1_000));
        let acct = account(13);

        // 8 * 100 = 800 notional consumed by the first admission.
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            8,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        )
        .expect("first admission within budget");

        // 3 * 100 = 300 attempted; 800 + 300 > 1_000 → reject.
        let rejected_id = Id::new_uuid();
        let result = book.add_limit_order_with_user(
            rejected_id,
            100,
            3,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        );
        assert!(
            matches!(result, Err(OrderBookError::RiskMaxNotional { .. })),
            "expected RiskMaxNotional, got {result:?}"
        );

        let status = book.order_status(rejected_id);
        assert_eq!(
            status,
            Some(OrderStatus::Rejected {
                reason: RejectReason::RiskMaxNotional,
            }),
            "expected Rejected{{RiskMaxNotional}}, got {status:?}"
        );
    }

    #[test]
    fn risk_price_band_reject_records_risk_price_band_in_tracker() {
        let mut book = book_with_tracker();
        seed_last_trade_price(&book, 1_000_000);
        // 1000 bps = 10% allowed band.
        book.set_risk_config(
            RiskConfig::new().with_price_band_bps(1_000, ReferencePriceSource::LastTrade),
        );

        // +30% from reference → rejected.
        let rejected_id = Id::new_uuid();
        let result =
            book.add_limit_order(rejected_id, 1_300_000, 1, Side::Buy, TimeInForce::Gtc, None);
        assert!(
            matches!(result, Err(OrderBookError::RiskPriceBand { .. })),
            "expected RiskPriceBand, got {result:?}"
        );

        let status = book.order_status(rejected_id);
        assert_eq!(
            status,
            Some(OrderStatus::Rejected {
                reason: RejectReason::RiskPriceBand,
            }),
            "expected Rejected{{RiskPriceBand}}, got {status:?}"
        );
    }

    // ───────────────────────────────────────────────────────────────────
    // Display sanity
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn display_format_round_trips_through_to_string() {
        // Sanity for log lines / wire-text rendering. One assertion per
        // variant covered by production emission paths after issue #55.
        assert_eq!(
            RejectReason::KillSwitchActive.to_string(),
            "kill switch active"
        );
        assert_eq!(
            RejectReason::RiskMaxOpenOrders.to_string(),
            "risk: max open orders"
        );
        assert_eq!(
            RejectReason::RiskMaxNotional.to_string(),
            "risk: max notional"
        );
        assert_eq!(RejectReason::RiskPriceBand.to_string(), "risk: price band");
        assert_eq!(
            RejectReason::PostOnlyWouldCross.to_string(),
            "post-only would cross"
        );
        assert_eq!(RejectReason::InvalidPrice.to_string(), "invalid price");
        assert_eq!(RejectReason::MissingUserId.to_string(), "missing user id");
        assert_eq!(RejectReason::Other(42).to_string(), "other(42)");
    }
}
