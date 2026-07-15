//! #210: two-tranche (Iceberg / Reserve) quantity conservation.
//!
//! - An aggressive two-tranche order's residual rests with the exact total
//!   remainder distributed across tranches — never the old
//!   "total-into-visible, hidden kept" inflation that manufactured
//!   liquidity.
//! - A `visible + hidden` total that overflows `u64` is rejected with the
//!   typed `QuantityOverflow` before any trade, listener, or book mutation.
//! - Conservation invariant (property-tested): for every aggressive
//!   iceberg submit, `executed + resting visible + resting hidden ==
//!   submitted visible + submitted hidden`.

#[cfg(test)]
mod tests_two_tranche_conservation {
    use orderbook_rs::{DefaultOrderBook, OrderBook, OrderBookError};
    use pricelevel::{Id, Side, TimeInForce};
    use proptest::prelude::*;

    const PRICE: u128 = 100;
    const MAKER_ID: u64 = 1;
    const ICEBERG_ID: u64 = 2;

    /// Rests `contra` sell units at `PRICE`, submits an aggressive iceberg
    /// buy (`visible`/`hidden`) at the same price, and returns
    /// `(executed, resting_visible, resting_hidden)`.
    fn run_iceberg_cross(
        contra: u64,
        visible: u64,
        hidden: u64,
    ) -> Result<(u64, u64, u64), TestCaseError> {
        let book: OrderBook<()> = DefaultOrderBook::new("CONS");
        if contra > 0
            && let Err(error) = book.add_limit_order(
                Id::from_u64(MAKER_ID),
                PRICE,
                contra,
                Side::Sell,
                TimeInForce::Gtc,
                None,
            )
        {
            return Err(TestCaseError::fail(format!("seed contra failed: {error}")));
        }

        if let Err(error) = book.add_iceberg_order(
            Id::from_u64(ICEBERG_ID),
            PRICE,
            visible,
            hidden,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        ) {
            return Err(TestCaseError::fail(format!(
                "iceberg submit failed: {error}"
            )));
        }

        let executed = contra.min(visible + hidden);
        let resting = book.get_order(Id::from_u64(ICEBERG_ID));
        let (rest_visible, rest_hidden) = match resting {
            Some(order) => (
                order.visible_quantity().as_u64(),
                order.hidden_quantity().as_u64(),
            ),
            None => (0, 0),
        };
        Ok((executed, rest_visible, rest_hidden))
    }

    /// The issue's repro: iceberg 20 visible + 80 hidden aggressed by 10
    /// contra units must rest exactly 90 total (20 visible / 70 hidden) —
    /// not the pre-#210 `visible=90, hidden=80` inflation.
    #[test]
    fn iceberg_partial_fill_rests_exact_total() {
        let (executed, visible, hidden) = match run_iceberg_cross(10, 20, 80) {
            Ok(v) => v,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(executed, 10);
        assert_eq!(visible, 20, "display tranche stays at the submitted size");
        assert_eq!(hidden, 70, "hidden absorbs the fill");
        assert_eq!(
            visible + hidden,
            90,
            "exactly the unmatched remainder rests"
        );
    }

    /// Fill smaller than, equal to, and larger than the visible tranche.
    #[test]
    fn iceberg_residual_covers_all_fill_positions() {
        // Fill (5) < visible (20): full display remains, hidden shrinks.
        let (_, v, h) = match run_iceberg_cross(5, 20, 80) {
            Ok(x) => x,
            Err(e) => panic!("{e}"),
        };
        assert_eq!((v, h), (20, 75));

        // Fill (20) == visible: remainder 80 rests as one display + hidden.
        let (_, v, h) = match run_iceberg_cross(20, 20, 80) {
            Ok(x) => x,
            Err(e) => panic!("{e}"),
        };
        assert_eq!((v, h), (20, 60));

        // Fill (50) > visible: remainder 50 rests as display 20 + hidden 30.
        let (_, v, h) = match run_iceberg_cross(50, 20, 80) {
            Ok(x) => x,
            Err(e) => panic!("{e}"),
        };
        assert_eq!((v, h), (20, 30));

        // Remainder smaller than the display: everything visible.
        let (_, v, h) = match run_iceberg_cross(90, 20, 80) {
            Ok(x) => x,
            Err(e) => panic!("{e}"),
        };
        assert_eq!((v, h), (10, 0));
    }

    /// An iceberg whose `visible + hidden` overflows `u64` is rejected with
    /// the typed error BEFORE anything executes: the contra maker is left
    /// fully intact and no trade is emitted.
    #[test]
    fn overflowing_two_tranche_total_rejected_pre_trade() {
        let book: OrderBook<()> = DefaultOrderBook::new("OVF");
        book.add_limit_order(
            Id::from_u64(MAKER_ID),
            PRICE,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("seed contra");

        let err = book
            .add_iceberg_order(
                Id::from_u64(ICEBERG_ID),
                PRICE,
                u64::MAX,
                1,
                Side::Buy,
                TimeInForce::Gtc,
                None,
            )
            .expect_err("overflowing total must be rejected");
        match err {
            OrderBookError::QuantityOverflow { visible, hidden } => {
                assert_eq!(visible, u64::MAX);
                assert_eq!(hidden, 1);
            }
            other => panic!("expected QuantityOverflow, got {other:?}"),
        }

        // Nothing executed, nothing mutated.
        assert!(book.last_trade_price().is_none(), "no trade may be emitted");
        let maker = book
            .get_order(Id::from_u64(MAKER_ID))
            .expect("contra maker still resting");
        assert_eq!(maker.visible_quantity().as_u64(), 10, "maker untouched");
        assert!(
            book.get_order(Id::from_u64(ICEBERG_ID)).is_none(),
            "rejected order never rests"
        );
    }

    /// Snapshot round-trip preserves the corrected residual tranches.
    #[test]
    fn corrected_residual_survives_snapshot_round_trip() {
        let book: OrderBook<()> = DefaultOrderBook::new("RT");
        book.add_limit_order(
            Id::from_u64(MAKER_ID),
            PRICE,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("seed contra");
        book.add_iceberg_order(
            Id::from_u64(ICEBERG_ID),
            PRICE,
            20,
            80,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        )
        .expect("iceberg submit");

        let package = book.create_snapshot_package(10).expect("package");
        let mut restored: OrderBook<()> = DefaultOrderBook::new("RT");
        restored
            .restore_from_snapshot_package(package)
            .expect("restore");

        let order = restored
            .get_order(Id::from_u64(ICEBERG_ID))
            .expect("residual restored");
        assert_eq!(order.visible_quantity().as_u64(), 20);
        assert_eq!(order.hidden_quantity().as_u64(), 70);
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, max_shrink_iters: 50_000, ..ProptestConfig::default() })]

        /// Conservation: for every aggressive iceberg submit,
        /// `executed + resting visible + resting hidden` equals the
        /// submitted `visible + hidden`.
        #[test]
        fn test_iceberg_cross_conserves_total_quantity(
            contra in 0u64..=200,
            visible in 1u64..=100,
            hidden in 0u64..=100,
        ) {
            let (executed, rest_visible, rest_hidden) =
                run_iceberg_cross(contra, visible, hidden)?;
            prop_assert_eq!(
                executed + rest_visible + rest_hidden,
                visible + hidden,
                "executed {} + resting {}v/{}h must equal submitted {}v/{}h",
                executed, rest_visible, rest_hidden, visible, hidden
            );
        }
    }
}

/// #210 follow-ups from review: the overflow rejection must win over the
/// risk gate (which would otherwise judge the saturated total), and the
/// Reserve residual path conserves quantity like the Iceberg one.
#[cfg(test)]
mod tests_two_tranche_review_gaps {
    use orderbook_rs::orderbook::risk::RiskConfig;
    use orderbook_rs::{DefaultOrderBook, OrderBook, OrderBookError};
    use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};

    /// With a RiskConfig installed, an overflowing iceberg must still be
    /// rejected as `QuantityOverflow` — not as a risk-family error derived
    /// from the saturated `u64::MAX` total.
    #[test]
    fn overflow_beats_risk_gate() {
        let mut book: OrderBook<()> = DefaultOrderBook::new("OVR");
        book.set_risk_config(RiskConfig::new().with_max_notional_per_account(1_000_000));

        let err = book
            .add_iceberg_order(
                Id::from_u64(1),
                100,
                u64::MAX,
                1,
                Side::Buy,
                TimeInForce::Gtc,
                None,
            )
            .expect_err("overflowing total must be rejected");
        assert!(
            matches!(err, OrderBookError::QuantityOverflow { .. }),
            "expected QuantityOverflow before the risk gate, got {err:?}"
        );
    }

    /// Reserve residual conservation: an aggressive reserve partially
    /// filled rests exactly the unmatched remainder across tranches (the
    /// visible-first-with-replenish reduction the user-facing update
    /// already used).
    #[test]
    fn reserve_partial_fill_conserves_total() {
        let book: OrderBook<()> = DefaultOrderBook::new("RSV");
        book.add_limit_order(Id::from_u64(1), 100, 10, Side::Sell, TimeInForce::Gtc, None)
            .expect("seed contra");

        let reserve = OrderType::ReserveOrder {
            id: Id::from_u64(2),
            price: Price::new(100),
            visible_quantity: Quantity::new(20),
            hidden_quantity: Quantity::new(80),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(1_700_000_000_000),
            time_in_force: TimeInForce::Gtc,
            replenish_threshold: Quantity::new(1),
            replenish_amount: None,
            auto_replenish: true,
            extra_fields: (),
        };
        book.add_order(reserve).expect("reserve submit");

        let resting = book.get_order(Id::from_u64(2)).expect("residual rests");
        let visible = resting.visible_quantity().as_u64();
        let hidden = resting.hidden_quantity().as_u64();
        assert_eq!(
            visible + hidden,
            90,
            "reserve residual conserves the unmatched total, got {visible}v/{hidden}h"
        );
    }
}
