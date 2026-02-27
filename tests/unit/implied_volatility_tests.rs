//! Integration tests for implied volatility calculation.

use orderbook_rs::{IVConfig, IVParams, IVQuality, OrderBook, PriceSource, SolverConfig};
use pricelevel::{Id, Side, TimeInForce};

/// Creates a test order book with realistic option prices.
fn create_option_book(bid_price: u128, ask_price: u128) -> OrderBook<()> {
    let book = OrderBook::<()>::new("SPY-C-450-2024-03-15");

    // Add multiple bid orders at different quantities
    let _ = book.add_limit_order(Id::new(), bid_price, 50, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), bid_price, 30, Side::Buy, TimeInForce::Gtc, None);

    // Add multiple ask orders
    let _ = book.add_limit_order(Id::new(), ask_price, 40, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), ask_price, 60, Side::Sell, TimeInForce::Gtc, None);

    book
}

#[test]
fn test_iv_calculation_atm_call() {
    // ATM call option: S=100, K=100, T=90 days, r=5%
    // With 25% IV, BS price ≈ 5.45
    let book = create_option_book(540, 550); // $5.40 - $5.50

    let params = IVParams::call(100.0, 100.0, 90.0 / 365.0, 0.05);
    let config = IVConfig::default().with_price_scale(100.0);

    let result = book
        .implied_volatility_with_config(&params, PriceSource::MidPrice, &config)
        .expect("IV calculation should succeed");

    // IV should be around 25% (within reasonable range)
    assert!(
        result.iv > 0.15 && result.iv < 0.35,
        "IV {} should be between 15% and 35%",
        result.iv
    );
    assert!(result.is_acceptable_quality());
}

#[test]
fn test_iv_calculation_itm_put() {
    // ITM put option: S=450, K=460, T=30 days, r=5%
    let book = create_option_book(1200, 1220); // $12.00 - $12.20

    let params = IVParams::put(450.0, 460.0, 30.0 / 365.0, 0.05);
    let config = IVConfig::default().with_price_scale(100.0);

    let result = book
        .implied_volatility_with_config(&params, PriceSource::MidPrice, &config)
        .expect("IV calculation should succeed");

    // Should converge to a reasonable IV
    assert!(result.iv > 0.10 && result.iv < 1.0);
    assert!(result.iterations < 20);
}

#[test]
fn test_iv_calculation_otm_call() {
    // OTM call option: S=100, K=110, T=90 days, r=5%
    // OTM options have lower prices
    let book = create_option_book(195, 205); // $1.95 - $2.05 (tight spread)

    let params = IVParams::call(100.0, 110.0, 90.0 / 365.0, 0.05);
    let config = IVConfig::default().with_price_scale(100.0);

    let result = book
        .implied_volatility_with_config(&params, PriceSource::MidPrice, &config)
        .expect("IV calculation should succeed");

    assert!(result.iv > 0.0);
    assert!(result.iterations < 30);
}

#[test]
fn test_iv_quality_based_on_spread() {
    // Tight spread (< 1%)
    let tight_book = create_option_book(500, 504); // 0.8% spread
    let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
    // Use higher max_spread to allow all tests to pass
    let config = IVConfig::default()
        .with_price_scale(100.0)
        .with_max_spread(2000.0); // Allow up to 20% spread

    let result = tight_book
        .implied_volatility_with_config(&params, PriceSource::MidPrice, &config)
        .expect("IV calculation should succeed");

    assert_eq!(result.quality, IVQuality::High);

    // Medium spread (1-5%)
    let medium_book = create_option_book(500, 520); // ~4% spread
    let result = medium_book
        .implied_volatility_with_config(&params, PriceSource::MidPrice, &config)
        .expect("IV calculation should succeed");

    assert_eq!(result.quality, IVQuality::Medium);

    // Wide spread (> 5%)
    let wide_book = create_option_book(500, 560); // ~11% spread
    let result = wide_book
        .implied_volatility_with_config(&params, PriceSource::MidPrice, &config)
        .expect("IV calculation should succeed");

    assert_eq!(result.quality, IVQuality::Low);
}

#[test]
fn test_iv_with_different_price_sources() {
    let book = OrderBook::<()>::new("TEST-OPT");

    // Asymmetric liquidity: more on bid side
    let _ = book.add_limit_order(Id::new(), 500, 1000, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 520, 100, Side::Sell, TimeInForce::Gtc, None);

    let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
    let config = IVConfig::default().with_price_scale(100.0);

    // Mid price
    let mid_result = book
        .implied_volatility_with_config(&params, PriceSource::MidPrice, &config)
        .expect("IV calculation should succeed");

    // Weighted mid (should be closer to bid due to more liquidity there)
    let weighted_result = book
        .implied_volatility_with_config(&params, PriceSource::WeightedMid, &config)
        .expect("IV calculation should succeed");

    // Weighted mid price should be different from simple mid
    // Due to asymmetric liquidity, weighted mid should be closer to ask
    // (inverse weighting: more bid qty means more weight on ask)
    assert!(
        (mid_result.price_used - weighted_result.price_used).abs() > 0.01,
        "Weighted mid should differ from simple mid"
    );
}

#[test]
fn test_iv_calculation_various_maturities() {
    let book = create_option_book(500, 510);
    let config = IVConfig::default().with_price_scale(100.0);

    // Test different maturities
    for days in [7, 14, 30, 60, 90, 180, 365] {
        let time = days as f64 / 365.0;
        let params = IVParams::call(100.0, 100.0, time, 0.05);

        let result = book.implied_volatility_with_config(&params, PriceSource::MidPrice, &config);

        // Should succeed for all reasonable maturities
        assert!(
            result.is_ok(),
            "IV calculation should succeed for {} days maturity",
            days
        );
    }
}

#[test]
fn test_iv_with_custom_solver_config() {
    let book = create_option_book(500, 510);

    let params = IVParams::call(100.0, 100.0, 0.25, 0.05);

    // Custom solver with tighter tolerance
    let solver = SolverConfig::new()
        .with_max_iterations(200)
        .with_tolerance(1e-10)
        .with_initial_guess(0.30);

    let config = IVConfig::default()
        .with_price_scale(100.0)
        .with_solver(solver);

    let result = book
        .implied_volatility_with_config(&params, PriceSource::MidPrice, &config)
        .expect("IV calculation should succeed");

    assert!(result.iv > 0.0);
}

#[test]
fn test_theoretical_price_and_greeks() {
    let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
    let vol = 0.25;

    // Test theoretical price
    let price = OrderBook::<()>::theoretical_price(&params, vol);
    assert!(price > 0.0);

    // Test Greeks
    let delta = OrderBook::<()>::option_delta(&params, vol);
    assert!(delta > 0.4 && delta < 0.6, "ATM call delta should be ~0.5");

    let gamma = OrderBook::<()>::option_gamma(&params, vol);
    assert!(gamma > 0.0, "Gamma should be positive");

    let vega = OrderBook::<()>::option_vega(&params, vol);
    assert!(vega > 0.0, "Vega should be positive");

    let theta = OrderBook::<()>::option_theta(&params, vol);
    assert!(theta < 0.0, "Theta should be negative for long options");
}

#[test]
fn test_iv_result_methods() {
    let book = create_option_book(500, 510);
    let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
    let config = IVConfig::default().with_price_scale(100.0);

    let result = book
        .implied_volatility_with_config(&params, PriceSource::MidPrice, &config)
        .expect("IV calculation should succeed");

    // Test helper methods
    let iv_pct = result.iv_percent();
    assert!((iv_pct - result.iv * 100.0).abs() < 1e-10);

    // Quality checks
    if result.spread_bps < 100.0 {
        assert!(result.is_high_quality());
    }
    if result.spread_bps < 500.0 {
        assert!(result.is_acceptable_quality());
    }
}

#[test]
fn test_iv_params_moneyness() {
    // ITM call
    let itm_call = IVParams::call(110.0, 100.0, 0.25, 0.05);
    assert!(itm_call.is_itm());
    assert!(!itm_call.is_otm());
    assert!((itm_call.intrinsic_value() - 10.0).abs() < 1e-10);

    // OTM call
    let otm_call = IVParams::call(90.0, 100.0, 0.25, 0.05);
    assert!(otm_call.is_otm());
    assert!(!otm_call.is_itm());
    assert!(otm_call.intrinsic_value() < 1e-10);

    // ATM call
    let atm_call = IVParams::call(100.0, 100.0, 0.25, 0.05);
    assert!(atm_call.is_atm());

    // ITM put
    let itm_put = IVParams::put(90.0, 100.0, 0.25, 0.05);
    assert!(itm_put.is_itm());
    assert!((itm_put.intrinsic_value() - 10.0).abs() < 1e-10);

    // OTM put
    let otm_put = IVParams::put(110.0, 100.0, 0.25, 0.05);
    assert!(otm_put.is_otm());
}
