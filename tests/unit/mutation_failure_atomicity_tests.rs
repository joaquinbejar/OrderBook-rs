//! #211: pricelevel 0.9 mutation failures are atomic and observable.
//!
//! - A submit whose residual could not be admitted is rejected BEFORE the
//!   sweep emits any trade (headroom pre-check).
//! - `UpdateQuantity` is validate-first: projected tick/lot/min-max/
//!   two-tranche/risk violations and upstream `PriceLevelError`s surface
//!   as typed errors with the maker unchanged; `Ok(None)` means only that
//!   the order is absent.
//! - Risk counters follow applied quantity updates.

#[cfg(test)]
mod tests_mutation_failure_atomicity {
    use orderbook_rs::orderbook::risk::RiskConfig;
    use orderbook_rs::{DefaultOrderBook, OrderBook, OrderBookError, OrderBookSnapshot};
    use pricelevel::{
        Hash32, Id, OrderType, OrderUpdate, Price, PriceLevel, Quantity, Side, TimeInForce,
        TimestampMs,
    };

    /// A buy whose residual would overflow the same-side level's aggregate
    /// is rejected before any trade: the crossing ask stays fully intact.
    ///
    /// A live book can never lock (bid and ask at one price), so the repro
    /// state — a near-capacity bid level AND a crossing ask at the same
    /// price — is built via snapshot restore, exactly the disaster-recovery
    /// shape the issue describes.
    #[test]
    fn residual_headroom_rejects_before_any_trade() {
        let make_level = |order_id: u64, qty: u64, side: Side| {
            let level = PriceLevel::new(100);
            let admitted = level.add_order(OrderType::Standard {
                id: Id::from_u64(order_id),
                price: Price::new(100),
                quantity: Quantity::new(qty),
                side,
                user_id: Hash32::zero(),
                timestamp: TimestampMs::new(1_700_000_000_000),
                time_in_force: TimeInForce::Gtc,
                extra_fields: (),
            });
            assert!(admitted.is_ok(), "fixture level admits order {order_id}");
            level.snapshot()
        };

        let book: OrderBook<()> = DefaultOrderBook::new("HEAD");
        book.restore_from_snapshot(OrderBookSnapshot {
            symbol: "HEAD".to_string(),
            timestamp: 1_700_000_000_000,
            bids: vec![make_level(1, u64::MAX - 2, Side::Buy)],
            asks: vec![make_level(2, 5, Side::Sell)],
        })
        .expect("restore locked book");

        // Buy 10 @ 100 would fill 5 from the ask, then rest 5 into the
        // bid level — whose aggregate (u64::MAX - 2) cannot absorb it.
        // The headroom pre-check must reject BEFORE the fill happens.
        let err = book
            .add_limit_order(Id::from_u64(3), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect_err("residual could not rest; submit must be rejected pre-trade");
        assert!(
            matches!(err, OrderBookError::InvalidOperation { .. }),
            "expected the typed capacity rejection, got {err:?}"
        );

        // Zero trades: the crossing ask is untouched and no last trade exists.
        assert!(book.last_trade_price().is_none(), "no trade was emitted");
        let ask = book.get_order(Id::from_u64(2)).expect("ask still resting");
        assert_eq!(ask.visible_quantity().as_u64(), 5, "ask fully intact");
        assert!(
            book.get_order(Id::from_u64(3)).is_none(),
            "rejected taker never rests"
        );
    }

    /// `UpdateQuantity` enforces the projected lot-size rule and leaves the
    /// maker unchanged on rejection.
    #[test]
    fn update_quantity_enforces_lot_size_on_projected_state() {
        let mut book: OrderBook<()> = DefaultOrderBook::new("LOTU");
        book.set_lot_size(5);
        book.add_limit_order(Id::from_u64(1), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed maker");

        let err = book
            .update_order(OrderUpdate::UpdateQuantity {
                order_id: Id::from_u64(1),
                new_quantity: Quantity::new(7),
            })
            .expect_err("off-lot projected quantity must be rejected");
        assert!(
            matches!(err, OrderBookError::InvalidLotSize { .. }),
            "expected InvalidLotSize, got {err:?}"
        );

        let maker = book.get_order(Id::from_u64(1)).expect("maker still rests");
        assert_eq!(
            maker.visible_quantity().as_u64(),
            10,
            "rejected update leaves the maker unchanged"
        );
    }

    /// `UpdateQuantity` enforces min/max order size on the projected state.
    #[test]
    fn update_quantity_enforces_max_order_size() {
        let mut book: OrderBook<()> = DefaultOrderBook::new("MAXU");
        book.set_max_order_size(50);
        book.add_limit_order(Id::from_u64(1), 100, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed maker");

        let err = book
            .update_order(OrderUpdate::UpdateQuantity {
                order_id: Id::from_u64(1),
                new_quantity: Quantity::new(60),
            })
            .expect_err("oversized projected quantity must be rejected");
        assert!(
            matches!(err, OrderBookError::OrderSizeOutOfRange { .. }),
            "expected OrderSizeOutOfRange, got {err:?}"
        );
        let maker = book.get_order(Id::from_u64(1)).expect("maker still rests");
        assert_eq!(maker.visible_quantity().as_u64(), 10);
    }

    /// `UpdateQuantity` enforces the modify-aware risk gate on the
    /// projected notional and leaves the maker unchanged on rejection.
    #[test]
    fn update_quantity_enforces_risk_notional() {
        let mut book: OrderBook<()> = DefaultOrderBook::new("RSKU");
        book.set_risk_config(RiskConfig::new().with_max_notional_per_account(2_000));
        let user = pricelevel::Hash32::new([9u8; 32]);
        book.add_limit_order_with_user(
            Id::from_u64(1),
            100,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            user,
            None,
        )
        .expect("seed maker within notional");

        // Projected notional 100 * 30 = 3000 > 2000.
        let err = book
            .update_order(OrderUpdate::UpdateQuantity {
                order_id: Id::from_u64(1),
                new_quantity: Quantity::new(30),
            })
            .expect_err("projected notional breach must be rejected");
        assert!(
            matches!(err, OrderBookError::RiskMaxNotional { .. }),
            "expected RiskMaxNotional, got {err:?}"
        );
        let maker = book.get_order(Id::from_u64(1)).expect("maker still rests");
        assert_eq!(maker.visible_quantity().as_u64(), 10);
    }

    /// A successful `UpdateQuantity` updates the account's risk counters:
    /// a follow-up admission that only fits after the decrease succeeds.
    #[test]
    fn update_quantity_releases_risk_notional_on_success() {
        let mut book: OrderBook<()> = DefaultOrderBook::new("RSKD");
        book.set_risk_config(RiskConfig::new().with_max_notional_per_account(2_000));
        let user = pricelevel::Hash32::new([9u8; 32]);
        book.add_limit_order_with_user(
            Id::from_u64(1),
            100,
            15,
            Side::Buy,
            TimeInForce::Gtc,
            user,
            None,
        )
        .expect("seed maker: notional 1500");

        // 1500 resting + 1000 attempted > 2000 → rejected while unchanged.
        let attempted = book.add_limit_order_with_user(
            Id::from_u64(2),
            100,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            user,
            None,
        );
        assert!(
            matches!(attempted, Err(OrderBookError::RiskMaxNotional { .. })),
            "second order must not fit before the decrease"
        );

        // Decrease to 5 (notional 500) — the applied update must release
        // 1000 notional in the account counters.
        let updated = book
            .update_order(OrderUpdate::UpdateQuantity {
                order_id: Id::from_u64(1),
                new_quantity: Quantity::new(5),
            })
            .expect("decrease succeeds");
        assert!(updated.is_some(), "maker was updated in place");

        // Now 500 resting + 1000 attempted <= 2000 → admitted.
        book.add_limit_order_with_user(
            Id::from_u64(2),
            100,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            user,
            None,
        )
        .expect("second order fits after the counter release");
    }

    /// An `UpdateQuantity` whose projected two-tranche total overflows is
    /// rejected with the typed `QuantityOverflow`.
    #[test]
    fn update_quantity_rejects_projected_overflow() {
        let book: OrderBook<()> = DefaultOrderBook::new("OVFU");
        book.add_iceberg_order(
            Id::from_u64(1),
            100,
            10,
            50,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        )
        .expect("seed iceberg");

        // Projected: visible = u64::MAX, hidden = 50 → overflow.
        let err = book
            .update_order(OrderUpdate::UpdateQuantity {
                order_id: Id::from_u64(1),
                new_quantity: Quantity::new(u64::MAX),
            })
            .expect_err("projected overflow must be rejected");
        assert!(
            matches!(err, OrderBookError::QuantityOverflow { .. }),
            "expected QuantityOverflow, got {err:?}"
        );
        let maker = book.get_order(Id::from_u64(1)).expect("maker still rests");
        assert_eq!(maker.visible_quantity().as_u64(), 10);
        assert_eq!(maker.hidden_quantity().as_u64(), 50);
    }

    /// `Ok(None)` is reserved for a genuinely absent order.
    #[test]
    fn update_quantity_absent_order_is_ok_none() {
        let book: OrderBook<()> = DefaultOrderBook::new("NONE");
        let result = book
            .update_order(OrderUpdate::UpdateQuantity {
                order_id: Id::from_u64(404),
                new_quantity: Quantity::new(5),
            })
            .expect("absent order is not an error");
        assert!(result.is_none(), "absent order reports Ok(None)");
    }
}

/// #211 review follow-ups: upstream error propagation on increase, the
/// counter-increase branch of the risk hook, and the projected-expiry
/// rejection that riding on the shared validator introduces.
#[cfg(test)]
mod tests_mutation_failure_review_gaps {
    use orderbook_rs::orderbook::risk::RiskConfig;
    use orderbook_rs::{Clock, DefaultOrderBook, OrderBook, OrderBookError, StubClock};
    use pricelevel::{Id, OrderUpdate, Quantity, Side, TimeInForce};
    use std::sync::Arc;

    /// Book-level validation passes but pricelevel's checked aggregate
    /// rejects the increase: the upstream `PriceLevelError` must surface
    /// (not `Ok(None)`) and the maker must be unchanged.
    #[test]
    fn update_quantity_propagates_upstream_counter_overflow() {
        let book: OrderBook<()> = DefaultOrderBook::new("UPST");
        // Same-side level aggregate: (MAX - 10) + 5 = MAX - 5.
        book.add_limit_order(
            Id::from_u64(1),
            100,
            u64::MAX - 10,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        )
        .expect("seed large maker");
        book.add_limit_order(Id::from_u64(2), 100, 5, Side::Buy, TimeInForce::Gtc, None)
            .expect("seed small maker");

        // Projected order (quantity 20) passes every book-level rule, but
        // the level cannot absorb +15 more units.
        let err = book
            .update_order(OrderUpdate::UpdateQuantity {
                order_id: Id::from_u64(2),
                new_quantity: Quantity::new(20),
            })
            .expect_err("upstream counter overflow must surface");
        assert!(
            matches!(err, OrderBookError::PriceLevelError(_)),
            "expected PriceLevelError, got {err:?}"
        );
        let maker = book.get_order(Id::from_u64(2)).expect("maker still rests");
        assert_eq!(maker.visible_quantity().as_u64(), 5, "maker unchanged");
    }

    /// The increase branch of `on_quantity_update`: growing a resting
    /// order books additional notional, so a follow-up admission that fit
    /// before the increase is now rejected.
    #[test]
    fn update_quantity_books_risk_notional_on_increase() {
        let mut book: OrderBook<()> = DefaultOrderBook::new("RSKI");
        book.set_risk_config(RiskConfig::new().with_max_notional_per_account(2_000));
        let user = pricelevel::Hash32::new([9u8; 32]);
        book.add_limit_order_with_user(
            Id::from_u64(1),
            100,
            5,
            Side::Buy,
            TimeInForce::Gtc,
            user,
            None,
        )
        .expect("seed maker: notional 500");

        // Increase to 15 → notional 1500 booked against the account.
        book.update_order(OrderUpdate::UpdateQuantity {
            order_id: Id::from_u64(1),
            new_quantity: Quantity::new(15),
        })
        .expect("increase within limits succeeds");

        // 1500 resting + 1000 attempted > 2000 → rejected only if the
        // increase was actually booked by the risk hook.
        let attempted = book.add_limit_order_with_user(
            Id::from_u64(2),
            100,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            user,
            None,
        );
        assert!(
            matches!(attempted, Err(OrderBookError::RiskMaxNotional { .. })),
            "the booked increase must gate the next admission, got {attempted:?}"
        );
    }

    /// An expired-but-unevicted GTD maker can no longer be resized: the
    /// projected order fails the shared validator's expiry rule.
    #[test]
    fn update_quantity_rejects_expired_gtd_maker() {
        let clock = Arc::new(StubClock::starting_at(1_000));
        let shared: Arc<dyn Clock> = clock.clone();
        let book: OrderBook<()> = OrderBook::with_clock("EXPU", shared.clone());

        // GTD maker expiring at t = 2_000; admitted while valid.
        book.add_limit_order(
            Id::from_u64(1),
            100,
            10,
            Side::Buy,
            TimeInForce::Gtd(2_000),
            None,
        )
        .expect("seed unexpired GTD maker");

        // Advance the stub clock past the deadline (each read ticks 1 ms).
        while clock.peek() <= 2_000 {
            let _ = shared.now_millis();
        }

        let err = book
            .update_order(OrderUpdate::UpdateQuantity {
                order_id: Id::from_u64(1),
                new_quantity: Quantity::new(5),
            })
            .expect_err("resizing an expired maker must be rejected");
        assert!(
            matches!(err, OrderBookError::InvalidOperation { .. }),
            "expected the expiry rejection, got {err:?}"
        );
        let maker = book.get_order(Id::from_u64(1)).expect("maker still rests");
        assert_eq!(maker.visible_quantity().as_u64(), 10, "maker unchanged");
    }
}
