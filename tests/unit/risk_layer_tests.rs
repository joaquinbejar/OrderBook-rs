//! Integration tests for the pre-trade risk layer on `OrderBook<T>`.
//!
//! Commit 2 of issue #54 covers the rejection paths that work with
//! the just-installed config: price-band breach, mid-fallback to
//! last-trade, no-reference fallthrough, market-order bypass, and
//! the public `set_risk_config` / `risk_config` / `disable_risk`
//! round-trip. Tests that depend on per-account counter state being
//! populated across submits (max_open_orders, max_notional, fill /
//! cancel deltas) land with commit 3, where `on_admission`,
//! `on_fill`, and `on_cancel` are wired into the engine.

#[cfg(test)]
mod tests_risk_layer {
    use orderbook_rs::{
        OrderBook, OrderBookError, ReferencePriceSource, RiskConfig,
    };
    use pricelevel::{Hash32, Id, Side, TimeInForce};

    fn new_book() -> OrderBook<()> {
        OrderBook::new("TEST")
    }

    fn account(byte: u8) -> Hash32 {
        Hash32::new([byte; 32])
    }

    // ───────────────────────────────────────────────────────────────
    // Public API round-trip
    // ───────────────────────────────────────────────────────────────

    #[test]
    fn risk_config_set_get_disable_round_trip() {
        let mut book = new_book();
        assert!(book.risk_config().is_none());

        let cfg = RiskConfig::new()
            .with_max_open_orders_per_account(7)
            .with_max_notional_per_account(123_456)
            .with_price_band_bps(250, ReferencePriceSource::LastTrade);
        book.set_risk_config(cfg.clone());

        let installed = book.risk_config().expect("config installed");
        assert_eq!(installed.max_open_orders_per_account, Some(7));
        assert_eq!(installed.max_notional_per_account, Some(123_456));
        assert_eq!(installed.price_band_bps, Some(250));
        assert_eq!(
            installed.reference_price,
            Some(ReferencePriceSource::LastTrade)
        );

        book.disable_risk();
        assert!(book.risk_config().is_none());
    }

    // ───────────────────────────────────────────────────────────────
    // Price-band rejection paths
    // ───────────────────────────────────────────────────────────────

    /// Seed two crossing orders so a trade prints and `last_trade_price`
    /// is set. After the helper returns, the book has no resting
    /// orders.
    fn seed_last_trade_price(book: &OrderBook<()>, price: u128) {
        // Resting ask at `price`.
        book.add_limit_order(
            Id::new_uuid(),
            price,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("seed resting ask");
        // Aggressive buy crosses fully.
        book.add_limit_order(
            Id::new_uuid(),
            price,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        )
        .expect("aggressive buy fills the ask");
        assert_eq!(
            book.last_trade_price(),
            Some(price),
            "last trade must be set"
        );
    }

    #[test]
    fn limit_far_outside_price_band_returns_risk_price_band() {
        let mut book = new_book();
        seed_last_trade_price(&book, 1_000_000);
        // 1000 bps = 10% allowed band.
        book.set_risk_config(
            RiskConfig::new()
                .with_price_band_bps(1_000, ReferencePriceSource::LastTrade),
        );

        // Submit at +30% from reference → rejected.
        let result = book.add_limit_order(
            Id::new_uuid(),
            1_300_000,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );
        match result {
            Err(OrderBookError::RiskPriceBand {
                submitted,
                reference,
                deviation_bps,
                limit_bps,
            }) => {
                assert_eq!(submitted, 1_300_000);
                assert_eq!(reference, 1_000_000);
                assert_eq!(deviation_bps, 3_000);
                assert_eq!(limit_bps, 1_000);
            }
            other => panic!("expected RiskPriceBand, got {other:?}"),
        }
    }

    #[test]
    fn limit_within_price_band_succeeds() {
        let mut book = new_book();
        seed_last_trade_price(&book, 1_000_000);
        book.set_risk_config(
            RiskConfig::new()
                .with_price_band_bps(1_000, ReferencePriceSource::LastTrade),
        );

        // +5% from reference is well within the 10% band.
        let result = book.add_limit_order(
            Id::new_uuid(),
            1_050_000,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );
        assert!(result.is_ok(), "in-band order must be accepted; got {result:?}");
    }

    #[test]
    fn mid_reference_falls_back_to_last_trade_when_one_sided() {
        let mut book = new_book();
        // Seed a last trade and confirm.
        seed_last_trade_price(&book, 1_000_000);
        // Add a single bid so the book is one-sided (no asks).
        book.add_limit_order(
            Id::new_uuid(),
            999_000,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        )
        .expect("seed bid");
        assert!(book.best_ask().is_none(), "book must be one-sided");

        book.set_risk_config(
            RiskConfig::new().with_price_band_bps(500, ReferencePriceSource::Mid),
        );

        // +30% from last_trade (1.3M) → rejected because Mid falls
        // back to last_trade when the book is one-sided.
        let result = book.add_limit_order(
            Id::new_uuid(),
            1_300_000,
            1,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
        assert!(
            matches!(result, Err(OrderBookError::RiskPriceBand { .. })),
            "Mid reference should fall back to last_trade and reject; got {result:?}"
        );
    }

    #[test]
    fn band_skipped_with_warn_when_no_reference_available() {
        let mut book = new_book();
        // Empty book + no trades → no reference price exists.
        book.set_risk_config(
            RiskConfig::new().with_price_band_bps(100, ReferencePriceSource::Mid),
        );

        // Far-out price should NOT be rejected because no reference
        // is available; the band check is skipped (warn-once latch).
        let result = book.add_limit_order(
            Id::new_uuid(),
            999_999_999,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );
        assert!(
            result.is_ok(),
            "no-reference path must skip the band check; got {result:?}"
        );
    }

    // ───────────────────────────────────────────────────────────────
    // Market-order bypass
    // ───────────────────────────────────────────────────────────────

    #[test]
    fn market_orders_bypass_risk_checks() {
        let mut book = new_book();
        // Seed resting liquidity for both market-order calls BEFORE
        // installing the risk config, so the seeding limits aren't
        // themselves blocked by the gate we're about to configure.
        book.add_limit_order(
            Id::new_uuid(),
            1_000_000,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("seed resting ask 1");
        book.add_limit_order(
            Id::new_uuid(),
            1_000_000,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("seed resting ask 2");

        // Configure a band so tight any submitted limit price would
        // fail, plus zero open-orders / notional ceilings. Market
        // orders carry no submitted price and no resting contribution
        // so they must bypass every gate.
        book.set_risk_config(
            RiskConfig::new()
                .with_price_band_bps(1, ReferencePriceSource::LastTrade)
                .with_max_open_orders_per_account(0)
                .with_max_notional_per_account(0),
        );

        // No user_id → submit_market_order; should match against a
        // resting ask and not be rejected by any risk gate.
        let result = book.submit_market_order(Id::new_uuid(), 1, Side::Buy);
        assert!(
            result.is_ok(),
            "market orders must bypass risk checks; got {result:?}"
        );

        // With user_id variant: same story.
        let result = book.submit_market_order_with_user(
            Id::new_uuid(),
            1,
            Side::Buy,
            account(42),
        );
        assert!(
            result.is_ok(),
            "submit_market_order_with_user must bypass risk; got {result:?}"
        );
    }
}
