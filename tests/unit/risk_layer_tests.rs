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
    use orderbook_rs::{OrderBook, OrderBookError, ReferencePriceSource, RiskConfig};
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
        book.add_limit_order(Id::new_uuid(), price, 10, Side::Buy, TimeInForce::Gtc, None)
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
            RiskConfig::new().with_price_band_bps(1_000, ReferencePriceSource::LastTrade),
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
            RiskConfig::new().with_price_band_bps(1_000, ReferencePriceSource::LastTrade),
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
        assert!(
            result.is_ok(),
            "in-band order must be accepted; got {result:?}"
        );
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

        book.set_risk_config(RiskConfig::new().with_price_band_bps(500, ReferencePriceSource::Mid));

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
        book.set_risk_config(RiskConfig::new().with_price_band_bps(100, ReferencePriceSource::Mid));

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

    // ───────────────────────────────────────────────────────────────
    // Per-account counter state (commit 3 — admission/fill/cancel hooks)
    // ───────────────────────────────────────────────────────────────

    #[test]
    fn submit_above_max_open_orders_returns_risk_max_open() {
        let mut book = new_book();
        book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(2));
        let acct = account(11);

        // Two admissions consume the quota.
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        )
        .expect("first order admitted");
        book.add_limit_order_with_user(
            Id::new_uuid(),
            101,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        )
        .expect("second order admitted");

        // Third is rejected.
        let result = book.add_limit_order_with_user(
            Id::new_uuid(),
            102,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        );
        match result {
            Err(OrderBookError::RiskMaxOpenOrders {
                account: a,
                current,
                limit,
            }) => {
                assert_eq!(a, acct);
                assert_eq!(current, 2);
                assert_eq!(limit, 2);
            }
            other => panic!("expected RiskMaxOpenOrders, got {other:?}"),
        }
    }

    #[test]
    fn submit_within_max_open_orders_succeeds() {
        let mut book = new_book();
        book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(3));
        let acct = account(12);

        for i in 0..3 {
            book.add_limit_order_with_user(
                Id::new_uuid(),
                100 + i,
                1,
                Side::Buy,
                TimeInForce::Gtc,
                acct,
                None,
            )
            .unwrap_or_else(|err| panic!("admission {i} failed: {err:?}"));
        }
    }

    #[test]
    fn submit_above_max_notional_returns_risk_max_notional() {
        let mut book = new_book();
        // 1_000 notional ceiling per account.
        book.set_risk_config(RiskConfig::new().with_max_notional_per_account(1_000));
        let acct = account(13);

        // 8 * 100 = 800 notional consumed.
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
        let result = book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            3,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        );
        match result {
            Err(OrderBookError::RiskMaxNotional {
                account: a,
                current,
                attempted,
                limit,
            }) => {
                assert_eq!(a, acct);
                assert_eq!(current, 800);
                assert_eq!(attempted, 300);
                assert_eq!(limit, 1_000);
            }
            other => panic!("expected RiskMaxNotional, got {other:?}"),
        }
    }

    #[test]
    fn submit_within_max_notional_succeeds() {
        let mut book = new_book();
        book.set_risk_config(RiskConfig::new().with_max_notional_per_account(1_000));
        let acct = account(14);

        // 8 * 100 = 800 in budget.
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            8,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        )
        .expect("first within budget");

        // 2 * 100 = 200; 800 + 200 = 1_000, exactly at the limit, so
        // accepted (`current + attempted > limit` is the gate, strict).
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            2,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        )
        .expect("second hits ceiling exactly and is accepted");
    }

    #[test]
    fn cancel_decrements_counters() {
        let mut book = new_book();
        book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(1));
        let acct = account(15);

        let order_id = Id::new_uuid();
        book.add_limit_order_with_user(order_id, 100, 1, Side::Buy, TimeInForce::Gtc, acct, None)
            .expect("first admission");

        // Second is rejected because the quota is full.
        assert!(
            matches!(
                book.add_limit_order_with_user(
                    Id::new_uuid(),
                    100,
                    1,
                    Side::Buy,
                    TimeInForce::Gtc,
                    acct,
                    None,
                ),
                Err(OrderBookError::RiskMaxOpenOrders { .. })
            ),
            "second should be rejected"
        );

        // Cancel and retry; should now succeed.
        book.cancel_order(order_id)
            .expect("cancel returns Ok")
            .expect("cancel returns Some");
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        )
        .expect("cancel must drop the counter and re-open the slot");
    }

    #[test]
    fn partial_fill_decrements_notional_and_keeps_count() {
        let mut book = new_book();
        // High open ceiling, tight notional ceiling: we want the
        // partial fill to free notional headroom for a follow-up.
        book.set_risk_config(
            RiskConfig::new()
                .with_max_open_orders_per_account(10)
                .with_max_notional_per_account(2_000),
        );
        let maker_acct = account(16);
        let taker_acct = account(17);

        // Maker rests 10 @ 100 (1_000 notional).
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            maker_acct,
            None,
        )
        .expect("maker admitted");

        // Taker (different account) submits an aggressive sell that
        // partially fills the maker (qty 4 of 10 at price 100).
        book.submit_market_order_with_user(Id::new_uuid(), 4, Side::Sell, taker_acct)
            .expect("aggressive sell fills 4 of 10");

        // Maker now has 600 notional (6 * 100). New maker admission
        // for 14 * 100 = 1_400 notional must succeed: 600 + 1_400 =
        // 2_000 (== limit, accepted by strict `>` gate). A larger one
        // (15 * 100 = 1_500) would be rejected.
        book.add_limit_order_with_user(
            Id::new_uuid(),
            99,
            14,
            Side::Buy,
            TimeInForce::Gtc,
            maker_acct,
            None,
        )
        .expect("partial fill must free notional headroom");

        let breach = book.add_limit_order_with_user(
            Id::new_uuid(),
            98,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            maker_acct,
            None,
        );
        assert!(
            matches!(breach, Err(OrderBookError::RiskMaxNotional { .. })),
            "ceiling already hit; expected RiskMaxNotional, got {breach:?}"
        );
    }

    #[test]
    fn full_fill_decrements_open_count() {
        let mut book = new_book();
        book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(1));
        let maker_acct = account(18);
        let taker_acct = account(19);

        // Maker uses the only slot.
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            5,
            Side::Buy,
            TimeInForce::Gtc,
            maker_acct,
            None,
        )
        .expect("maker admitted");

        // Aggressive sell fully consumes the maker.
        book.submit_market_order_with_user(Id::new_uuid(), 5, Side::Sell, taker_acct)
            .expect("aggressive sell fills the maker fully");

        // Maker's slot must be free again.
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            maker_acct,
            None,
        )
        .expect("full fill must drop open_count and re-open the slot");
    }

    #[test]
    fn disable_risk_clears_gates_keeps_counters() {
        let mut book = new_book();
        book.set_risk_config(RiskConfig::new().with_max_open_orders_per_account(1));
        let acct = account(20);

        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        )
        .expect("first admitted");
        // Quota full → second rejected.
        assert!(
            matches!(
                book.add_limit_order_with_user(
                    Id::new_uuid(),
                    100,
                    1,
                    Side::Buy,
                    TimeInForce::Gtc,
                    acct,
                    None,
                ),
                Err(OrderBookError::RiskMaxOpenOrders { .. })
            ),
            "expected rejection at quota"
        );

        book.disable_risk();

        // After disable, gate is lifted and admission succeeds even
        // though the per-account counter still reads 1 underneath.
        book.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            acct,
            None,
        )
        .expect("disable_risk lifts the gate");
        assert!(book.risk_config().is_none());
    }

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
        let result = book.submit_market_order_with_user(Id::new_uuid(), 1, Side::Buy, account(42));
        assert!(
            result.is_ok(),
            "submit_market_order_with_user must bypass risk; got {result:?}"
        );
    }

    // ───────────────────────────────────────────────────────────────
    // Snapshot persistence (commit 4 — RiskConfig + counter rebuild)
    // ───────────────────────────────────────────────────────────────

    #[test]
    fn risk_config_round_trips_through_snapshot() {
        // Build the original book, install a fully-configured risk
        // layer, and rest a few orders across two accounts so the
        // per-account counters carry meaningful state.
        let mut original = new_book();
        let cfg = RiskConfig::new()
            .with_max_open_orders_per_account(2)
            .with_max_notional_per_account(1_000)
            .with_price_band_bps(5_000, ReferencePriceSource::LastTrade);
        original.set_risk_config(cfg.clone());

        let acct_a = account(31);
        let acct_b = account(32);

        // Account A: 2 resting orders @ price 100 — saturates the
        // open-orders quota for that account post-restore.
        original
            .add_limit_order_with_user(
                Id::new_uuid(),
                100,
                3,
                Side::Buy,
                TimeInForce::Gtc,
                acct_a,
                None,
            )
            .expect("acct_a first admission");
        original
            .add_limit_order_with_user(
                Id::new_uuid(),
                100,
                4,
                Side::Buy,
                TimeInForce::Gtc,
                acct_a,
                None,
            )
            .expect("acct_a second admission");

        // Account B: a single resting order — quota still has room.
        original
            .add_limit_order_with_user(
                Id::new_uuid(),
                100,
                2,
                Side::Buy,
                TimeInForce::Gtc,
                acct_b,
                None,
            )
            .expect("acct_b first admission");

        // JSON round-trip via the public snapshot API.
        let json_payload = original
            .snapshot_to_json(10)
            .expect("serialize snapshot package to JSON");

        let mut restored = new_book();
        restored
            .restore_from_snapshot_json(&json_payload)
            .expect("restore from JSON");

        // 1. Config round-trips field-by-field.
        let restored_cfg = restored.risk_config().expect("config restored");
        assert_eq!(
            restored_cfg.max_open_orders_per_account,
            cfg.max_open_orders_per_account,
        );
        assert_eq!(
            restored_cfg.max_notional_per_account,
            cfg.max_notional_per_account,
        );
        assert_eq!(restored_cfg.price_band_bps, cfg.price_band_bps);
        assert_eq!(restored_cfg.reference_price, cfg.reference_price);

        // 2. Account A saturated its quota pre-snapshot. A new
        // submission must be rejected by the rebuilt counters.
        let breach = restored.add_limit_order_with_user(
            Id::new_uuid(),
            100,
            1,
            Side::Buy,
            TimeInForce::Gtc,
            acct_a,
            None,
        );
        match breach {
            Err(OrderBookError::RiskMaxOpenOrders {
                account: a,
                current,
                limit,
            }) => {
                assert_eq!(a, acct_a);
                assert_eq!(current, 2);
                assert_eq!(limit, 2);
            }
            other => {
                panic!("expected RiskMaxOpenOrders for acct_a after restore, got {other:?}");
            }
        }

        // 3. Account B still has one slot of headroom.
        restored
            .add_limit_order_with_user(
                Id::new_uuid(),
                100,
                1,
                Side::Buy,
                TimeInForce::Gtc,
                acct_b,
                None,
            )
            .expect("acct_b within rebuilt quota must succeed");
    }

    #[test]
    fn legacy_v2_snapshot_without_risk_config_field_defaults_to_none() {
        use orderbook_rs::orderbook::OrderBookSnapshotPackage;

        // Hand-rolled v2 payload that omits the new `risk_config`
        // field. Deserialization must succeed via `#[serde(default)]`
        // and yield `risk_config: None`. The checksum corresponds to
        // the empty snapshot below; we only assert the additive field
        // default — checksum validation is exercised elsewhere.
        let legacy_v2 = r#"{
            "version": 2,
            "snapshot": {
                "symbol": "LEGACY",
                "timestamp": 0,
                "bids": [],
                "asks": []
            },
            "checksum": "0000000000000000000000000000000000000000000000000000000000000000",
            "fee_schedule": null,
            "stp_mode": "None",
            "tick_size": null,
            "lot_size": null,
            "min_order_size": null,
            "max_order_size": null,
            "engine_seq": 0,
            "kill_switch_engaged": false
        }"#;

        let pkg =
            OrderBookSnapshotPackage::from_json(legacy_v2).expect("legacy v2 payload deserializes");
        assert!(
            pkg.risk_config.is_none(),
            "missing risk_config must default to None for v2 payloads"
        );
        assert_eq!(pkg.version, 2);
    }
}
