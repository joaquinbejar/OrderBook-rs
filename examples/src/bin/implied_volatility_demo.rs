//! Implied Volatility Calculation Demo
//!
//! This example demonstrates how to calculate implied volatility (IV) from
//! option prices in an order book using the Black-Scholes model inversion.
//!
//! # Features Demonstrated
//! - Basic IV calculation from order book prices
//! - Different price sources (MidPrice, WeightedMid, LastTrade)
//! - IV quality assessment based on bid-ask spread
//! - Greeks calculation (delta, gamma, vega, theta)
//! - Custom solver configuration
//! - Error handling for edge cases
//!
//! # Run
//! ```bash
//! cargo run --bin implied_volatility_demo
//! ```

use orderbook_rs::{
    BlackScholes, IVConfig, IVError, IVParams, IVQuality, OrderBook, PriceSource, SolverConfig,
};
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() {
    setup_logger();
    info!("=== Implied Volatility Calculation Demo ===\n");

    // Demo 1: Basic IV calculation
    demo_basic_iv_calculation();

    // Demo 2: Different price sources
    demo_price_sources();

    // Demo 3: IV quality assessment
    demo_iv_quality();

    // Demo 4: Greeks calculation
    demo_greeks_calculation();

    // Demo 5: Custom solver configuration
    demo_custom_solver();

    // Demo 6: Error handling
    demo_error_handling();

    // Demo 7: IV surface construction (multiple strikes)
    demo_iv_surface();

    info!("\n=== Demo Complete ===");
}

/// Demonstrates basic IV calculation from order book prices.
fn demo_basic_iv_calculation() {
    info!("--- Demo 1: Basic IV Calculation ---\n");

    // Create an order book for an option contract
    // Symbol format: UNDERLYING-TYPE-STRIKE-EXPIRY
    let book = OrderBook::<()>::new("SPY-C-450-2024-03-15");

    // Add bid orders (buyers willing to pay)
    let _ = book.add_limit_order(
        Id::new(),
        920, // $9.20 (prices in cents)
        100,
        Side::Buy,
        TimeInForce::Gtc,
        None,
    );
    let _ = book.add_limit_order(
        Id::new(),
        915, // $9.15
        50,
        Side::Buy,
        TimeInForce::Gtc,
        None,
    );

    // Add ask orders (sellers asking price)
    let _ = book.add_limit_order(
        Id::new(),
        930, // $9.30
        80,
        Side::Sell,
        TimeInForce::Gtc,
        None,
    );
    let _ = book.add_limit_order(
        Id::new(),
        935, // $9.35
        120,
        Side::Sell,
        TimeInForce::Gtc,
        None,
    );

    // Define option parameters
    // ATM call: S=450, K=450, T=90 days, r=5%
    let params = IVParams::call(
        450.0,        // Spot price
        450.0,        // Strike price
        90.0 / 365.0, // Time to expiry (90 days in years)
        0.05,         // Risk-free rate (5%)
    );

    // Configure IV calculation
    // price_scale=100 because our prices are in cents
    let config = IVConfig::default().with_price_scale(100.0);

    // Calculate IV using mid price
    match book.implied_volatility_with_config(&params, PriceSource::MidPrice, &config) {
        Ok(result) => {
            info!("Option: SPY 450 Call, 90 DTE");
            info!("  Spot: ${:.2}", params.spot);
            info!("  Strike: ${:.2}", params.strike);
            info!(
                "  Time to Expiry: {:.1} days",
                params.time_to_expiry * 365.0
            );
            info!("  Risk-free Rate: {:.1}%", params.risk_free_rate * 100.0);
            info!("");
            info!("Market Data:");
            info!(
                "  Best Bid: ${:.2}",
                book.best_bid().unwrap_or(0) as f64 / 100.0
            );
            info!(
                "  Best Ask: ${:.2}",
                book.best_ask().unwrap_or(0) as f64 / 100.0
            );
            info!("  Mid Price: ${:.2}", result.price_used);
            info!("  Spread: {:.1} bps", result.spread_bps);
            info!("");
            info!("IV Calculation Result:");
            info!("  Implied Volatility: {:.2}%", result.iv_percent());
            info!("  Quality: {:?}", result.quality);
            info!("  Iterations: {}", result.iterations);
        }
        Err(e) => info!("Failed to calculate IV: {}", e),
    }

    info!("");
}

/// Demonstrates different price sources for IV calculation.
fn demo_price_sources() {
    info!("--- Demo 2: Different Price Sources ---\n");

    let book = OrderBook::<()>::new("AAPL-C-180-2024-04-19");

    // Create asymmetric liquidity (more on bid side)
    let _ = book.add_limit_order(Id::new(), 550, 500, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 545, 300, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 570, 100, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 575, 150, Side::Sell, TimeInForce::Gtc, None);

    // Execute a trade to set last trade price
    let _ = book.match_market_order(Id::new(), 50, Side::Buy);

    let params = IVParams::call(180.0, 180.0, 60.0 / 365.0, 0.05);
    let config = IVConfig::default().with_price_scale(100.0);

    info!("Order Book State:");
    info!("  Best Bid: ${:.2} (qty: 500)", 5.50);
    info!("  Best Ask: ${:.2} (qty: 100)", 5.70);
    info!("  Bid Liquidity: 800 contracts");
    info!("  Ask Liquidity: 250 contracts");
    info!("");

    // Calculate IV with different price sources
    let sources = [
        (PriceSource::MidPrice, "MidPrice"),
        (PriceSource::WeightedMid, "WeightedMid"),
        (PriceSource::LastTrade, "LastTrade"),
    ];

    for (source, name) in sources {
        match book.implied_volatility_with_config(&params, source, &config) {
            Ok(result) => {
                info!(
                    "  {}: Price=${:.2}, IV={:.2}%",
                    name,
                    result.price_used,
                    result.iv_percent()
                );
            }
            Err(e) => info!("  {}: Error - {}", name, e),
        }
    }

    info!("");
    info!("Note: WeightedMid gives more weight to the side with less liquidity,");
    info!("      so it's closer to the ask price due to more bid liquidity.");
    info!("");
}

/// Demonstrates IV quality assessment based on bid-ask spread.
fn demo_iv_quality() {
    info!("--- Demo 3: IV Quality Assessment ---\n");

    let params = IVParams::call(100.0, 100.0, 90.0 / 365.0, 0.05);

    // Test different spread scenarios
    let scenarios = [
        ("Tight Spread (<1%)", 500, 504, IVQuality::High),
        ("Medium Spread (1-5%)", 500, 515, IVQuality::Medium),
        ("Wide Spread (>5%)", 500, 540, IVQuality::Low),
    ];

    for (name, bid, ask, expected_quality) in scenarios {
        let book = OrderBook::<()>::new("TEST-OPT");
        let _ = book.add_limit_order(Id::new(), bid, 100, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), ask, 100, Side::Sell, TimeInForce::Gtc, None);

        let config = IVConfig::default()
            .with_price_scale(100.0)
            .with_max_spread(2000.0); // Allow wide spreads for demo

        match book.implied_volatility_with_config(&params, PriceSource::MidPrice, &config) {
            Ok(result) => {
                let spread_pct = (ask - bid) as f64 / ((bid + ask) as f64 / 2.0) * 100.0;
                info!(
                    "{}: Bid=${:.2}, Ask=${:.2}, Spread={:.1}%",
                    name,
                    bid as f64 / 100.0,
                    ask as f64 / 100.0,
                    spread_pct
                );
                info!(
                    "  IV={:.2}%, Quality={:?} (expected: {:?})",
                    result.iv_percent(),
                    result.quality,
                    expected_quality
                );
                info!(
                    "  is_high_quality={}, is_acceptable_quality={}",
                    result.is_high_quality(),
                    result.is_acceptable_quality()
                );
            }
            Err(e) => info!("{}: Error - {}", name, e),
        }
    }

    info!("");
    info!("Quality Thresholds:");
    info!("  High:   spread < 100 bps (1%)");
    info!("  Medium: spread < 500 bps (5%)");
    info!("  Low:    spread >= 500 bps");
    info!("");
}

/// Demonstrates Greeks calculation using Black-Scholes.
fn demo_greeks_calculation() {
    info!("--- Demo 4: Greeks Calculation ---\n");

    // ATM call option
    let call_params = IVParams::call(100.0, 100.0, 90.0 / 365.0, 0.05);
    let vol = 0.25; // 25% volatility

    info!("ATM Call Option (S=100, K=100, T=90d, r=5%, σ=25%):");
    info!("");

    // Theoretical price
    let price = OrderBook::<()>::theoretical_price(&call_params, vol);
    info!("  Theoretical Price: ${:.4}", price);

    // Greeks
    let delta = OrderBook::<()>::option_delta(&call_params, vol);
    let gamma = OrderBook::<()>::option_gamma(&call_params, vol);
    let vega = OrderBook::<()>::option_vega(&call_params, vol);
    let theta = OrderBook::<()>::option_theta(&call_params, vol);

    info!("");
    info!("Greeks:");
    info!(
        "  Delta: {:.4} (price change per $1 move in underlying)",
        delta
    );
    info!(
        "  Gamma: {:.4} (delta change per $1 move in underlying)",
        gamma
    );
    info!(
        "  Vega:  {:.4} (price change per 1% vol increase)",
        vega / 100.0
    );
    info!("  Theta: {:.4} (daily time decay)", theta);

    info!("");

    // Compare call vs put
    let put_params = IVParams::put(100.0, 100.0, 90.0 / 365.0, 0.05);
    let put_price = OrderBook::<()>::theoretical_price(&put_params, vol);
    let put_delta = OrderBook::<()>::option_delta(&put_params, vol);

    info!("ATM Put Option (same parameters):");
    info!("  Theoretical Price: ${:.4}", put_price);
    info!("  Delta: {:.4}", put_delta);
    info!("");
    info!("Put-Call Parity Check:");
    info!("  Call - Put = ${:.4}", price - put_price);
    info!(
        "  S - K*e^(-rT) = ${:.4}",
        100.0 - 100.0 * (-0.05 * 90.0 / 365.0_f64).exp()
    );
    info!("");
}

/// Demonstrates custom solver configuration.
fn demo_custom_solver() {
    info!("--- Demo 5: Custom Solver Configuration ---\n");

    let book = OrderBook::<()>::new("NVDA-C-500-2024-06-21");
    let _ = book.add_limit_order(Id::new(), 4500, 100, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 4550, 100, Side::Sell, TimeInForce::Gtc, None);

    let params = IVParams::call(500.0, 500.0, 180.0 / 365.0, 0.05);

    // Default solver
    let default_config = IVConfig::default().with_price_scale(100.0);

    // Custom solver with tighter tolerance
    let custom_solver = SolverConfig::new()
        .with_max_iterations(200)
        .with_tolerance(1e-10)
        .with_initial_guess(0.30)
        .with_bounds(0.01, 3.0);

    let custom_config = IVConfig::default()
        .with_price_scale(100.0)
        .with_solver(custom_solver);

    info!("Solver Configurations:");
    info!("");
    info!("Default Solver:");
    info!("  max_iterations: 100");
    info!("  tolerance: 1e-8");
    info!("  initial_guess: 0.25 (25%)");
    info!("  bounds: [0.001, 5.0]");

    if let Ok(result) =
        book.implied_volatility_with_config(&params, PriceSource::MidPrice, &default_config)
    {
        info!(
            "  Result: IV={:.6}%, iterations={}",
            result.iv_percent(),
            result.iterations
        );
    }

    info!("");
    info!("Custom Solver:");
    info!("  max_iterations: 200");
    info!("  tolerance: 1e-10");
    info!("  initial_guess: 0.30 (30%)");
    info!("  bounds: [0.01, 3.0]");

    if let Ok(result) =
        book.implied_volatility_with_config(&params, PriceSource::MidPrice, &custom_config)
    {
        info!(
            "  Result: IV={:.6}%, iterations={}",
            result.iv_percent(),
            result.iterations
        );
    }

    info!("");
}

/// Demonstrates error handling for edge cases.
fn demo_error_handling() {
    info!("--- Demo 6: Error Handling ---\n");

    // Error 1: No price available (empty book)
    info!("1. Empty Order Book:");
    let empty_book = OrderBook::<()>::new("EMPTY");
    let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
    let config = IVConfig::default();

    match empty_book.implied_volatility_with_config(&params, PriceSource::MidPrice, &config) {
        Err(IVError::NoPriceAvailable) => info!("   Error: NoPriceAvailable (expected)"),
        Err(e) => info!("   Error: {}", e),
        Ok(_) => info!("   Unexpected success"),
    }

    // Error 2: Spread too wide
    info!("");
    info!("2. Spread Too Wide:");
    let wide_book = OrderBook::<()>::new("WIDE");
    let _ = wide_book.add_limit_order(Id::new(), 100, 100, Side::Buy, TimeInForce::Gtc, None);
    let _ = wide_book.add_limit_order(Id::new(), 500, 100, Side::Sell, TimeInForce::Gtc, None);

    let strict_config = IVConfig::default()
        .with_price_scale(100.0)
        .with_max_spread(500.0); // 5% max spread

    match wide_book.implied_volatility_with_config(&params, PriceSource::MidPrice, &strict_config) {
        Err(IVError::SpreadTooWide {
            spread_bps,
            threshold_bps,
        }) => {
            info!(
                "   Error: SpreadTooWide (spread={:.0} bps > threshold={:.0} bps)",
                spread_bps, threshold_bps
            );
        }
        Err(e) => info!("   Error: {}", e),
        Ok(_) => info!("   Unexpected success"),
    }

    // Error 3: Price below intrinsic value
    info!("");
    info!("3. Price Below Intrinsic Value:");
    let arb_book = OrderBook::<()>::new("ARB");
    // ITM call with intrinsic value of $10, but market price is $5
    let _ = arb_book.add_limit_order(Id::new(), 490, 100, Side::Buy, TimeInForce::Gtc, None);
    let _ = arb_book.add_limit_order(Id::new(), 510, 100, Side::Sell, TimeInForce::Gtc, None);

    let itm_params = IVParams::call(110.0, 100.0, 0.25, 0.0); // Intrinsic = $10
    let config = IVConfig::default().with_price_scale(100.0);

    match arb_book.implied_volatility_with_config(&itm_params, PriceSource::MidPrice, &config) {
        Err(IVError::PriceBelowIntrinsic { price, intrinsic }) => {
            info!(
                "   Error: PriceBelowIntrinsic (price=${:.2} < intrinsic=${:.2})",
                price, intrinsic
            );
        }
        Err(e) => info!("   Error: {}", e),
        Ok(_) => info!("   Unexpected success"),
    }

    info!("");
}

/// Demonstrates IV surface construction across multiple strikes.
fn demo_iv_surface() {
    info!("--- Demo 7: IV Surface Construction ---\n");

    let spot = 100.0;
    let rate = 0.05;
    let base_vol = 0.25;

    // Create option books for different strikes
    let strikes = [90.0, 95.0, 100.0, 105.0, 110.0];
    let maturities = [30, 60, 90]; // days

    info!("Underlying: $100.00");
    info!("Risk-free Rate: 5%");
    info!("");
    info!("IV Surface (%):");
    info!("");

    // Print header
    print!("{:>10}", "Strike");
    for days in &maturities {
        print!("{:>10}d", days);
    }
    println!();
    println!("{}", "-".repeat(40));

    for &strike in &strikes {
        print!("{:>10.0}", strike);

        for &days in &maturities {
            let time = days as f64 / 365.0;
            let params = IVParams::call(spot, strike, time, rate);

            // Simulate volatility smile: higher IV for OTM options
            let moneyness = (spot / strike).ln();
            let smile_adjustment = 0.1 * moneyness.powi(2);
            let term_adjustment = 0.02 * (90.0 - days as f64) / 90.0;
            let simulated_vol = base_vol + smile_adjustment + term_adjustment;

            // Calculate theoretical price with simulated vol
            let price = BlackScholes::price(&params, simulated_vol);

            // Create order book with this price
            let book = OrderBook::<()>::new("TEMP");
            let price_cents: u128 = (price * 100.0) as u128;
            let spread: u128 = (price_cents / 50).max(1); // ~2% spread

            let _ = book.add_limit_order(
                Id::new(),
                price_cents.saturating_sub(spread),
                100,
                Side::Buy,
                TimeInForce::Gtc,
                None,
            );
            let _ = book.add_limit_order(
                Id::new(),
                price_cents + spread,
                100,
                Side::Sell,
                TimeInForce::Gtc,
                None,
            );

            let config = IVConfig::default().with_price_scale(100.0);

            match book.implied_volatility_with_config(&params, PriceSource::MidPrice, &config) {
                Ok(result) => print!("{:>10.1}", result.iv_percent()),
                Err(_) => print!("{:>10}", "N/A"),
            }
        }
        println!();
    }

    info!("");
    info!("Note: This demonstrates the volatility smile pattern where");
    info!("      OTM options (strikes far from spot) have higher IV.");
    info!("");
}
