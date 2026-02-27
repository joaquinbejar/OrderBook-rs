// examples/src/bin/aggregate_statistics.rs
//
// This example demonstrates aggregate statistics for order book analysis.
// These statistics help quantitative traders detect market conditions,
// identify trends and pressure, and make informed trading decisions.
//
// Functions demonstrated:
// - `depth_statistics()`: Comprehensive depth metrics (volume, sizes, std dev)
// - `buy_sell_pressure()`: Market pressure indicators
// - `is_thin_book()`: Liquidity health check
// - `depth_distribution()`: Histogram of liquidity distribution
// - `order_book_imbalance()`: Buy/sell imbalance (-1.0 to 1.0)
//
// Run this example with:
//   cargo run --bin aggregate_statistics
//   (from the examples directory)

use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() {
    // Set up logging
    setup_logger();
    info!("Aggregate Statistics Example");

    // Create order book with realistic depth
    let book = create_orderbook_with_depth("BTC/USD");

    // Display current state
    display_book_state(&book);

    // Demonstrate depth statistics
    demo_depth_statistics(&book);

    // Demonstrate market pressure analysis
    demo_market_pressure(&book);

    // Demonstrate liquidity checks
    demo_liquidity_health(&book);

    // Demonstrate distribution analysis
    demo_depth_distribution(&book);

    // Demonstrate imbalance detection
    demo_imbalance_detection(&book);

    // Practical trading scenarios
    demo_trading_scenarios(&book);
}

fn create_orderbook_with_depth(symbol: &str) -> OrderBook {
    info!("\n=== Creating OrderBook ===");
    info!("Symbol: {}", symbol);

    let book = OrderBook::new(symbol);

    // Add buy orders with varying sizes (simulate realistic market)
    info!("\nAdding buy orders (bids):");
    let bid_orders = vec![
        (50000, 10), // Best bid
        (49980, 25),
        (49950, 40),
        (49920, 30),
        (49900, 50),
        (49880, 35),
        (49850, 20),
        (49800, 15),
        (49750, 45),
        (49700, 60),
    ];

    for (price, quantity) in bid_orders {
        let _ = book.add_limit_order(
            Id::new(),
            price,
            quantity,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );
        info!("  {} @ {}", quantity, price);
    }

    // Add sell orders with varying sizes
    info!("\nAdding sell orders (asks):");
    let ask_orders = vec![
        (50050, 12), // Best ask
        (50100, 22),
        (50150, 35),
        (50200, 28),
        (50250, 45),
        (50300, 38),
        (50350, 25),
        (50400, 18),
        (50450, 40),
        (50500, 55),
    ];

    for (price, quantity) in ask_orders {
        let _ = book.add_limit_order(
            Id::new(),
            price,
            quantity,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
        info!("  {} @ {}", quantity, price);
    }

    book
}

fn display_book_state(book: &OrderBook) {
    info!("\n=== OrderBook State ===");

    if let (Some(best_bid), Some(best_ask)) = (book.best_bid(), book.best_ask()) {
        info!("Best Bid: {}", best_bid);
        info!("Best Ask: {}", best_ask);
        info!("Spread: {}", best_ask - best_bid);
        info!("Mid Price: {}", (best_bid + best_ask) / 2);
    }

    let (buy_volume, sell_volume) = book.buy_sell_pressure();
    info!("Total Buy Volume: {}", buy_volume);
    info!("Total Sell Volume: {}", sell_volume);
}

fn demo_depth_statistics(book: &OrderBook) {
    info!("\n=== Depth Statistics ===");
    info!("Analyzing top 5 levels on each side");

    // Analyze bid side
    info!("\n📊 Bid Side Statistics:");
    let bid_stats = book.depth_statistics(Side::Buy, 5);

    info!("  Total Volume: {}", bid_stats.total_volume);
    info!("  Levels Analyzed: {}", bid_stats.levels_count);
    info!("  Average Level Size: {:.2}", bid_stats.avg_level_size);
    info!("  Weighted Avg Price: {:.2}", bid_stats.weighted_avg_price);
    info!("  Min Level Size: {}", bid_stats.min_level_size);
    info!("  Max Level Size: {}", bid_stats.max_level_size);
    info!("  Std Dev of Sizes: {:.2}", bid_stats.std_dev_level_size);

    // Analyze ask side
    info!("\n📊 Ask Side Statistics:");
    let ask_stats = book.depth_statistics(Side::Sell, 5);

    info!("  Total Volume: {}", ask_stats.total_volume);
    info!("  Levels Analyzed: {}", ask_stats.levels_count);
    info!("  Average Level Size: {:.2}", ask_stats.avg_level_size);
    info!("  Weighted Avg Price: {:.2}", ask_stats.weighted_avg_price);
    info!("  Min Level Size: {}", ask_stats.min_level_size);
    info!("  Max Level Size: {}", ask_stats.max_level_size);
    info!("  Std Dev of Sizes: {:.2}", ask_stats.std_dev_level_size);

    // Interpret statistics
    info!("\n💡 Interpretation:");

    if bid_stats.std_dev_level_size > bid_stats.avg_level_size * 0.5 {
        info!("  ⚠️  High variability in bid sizes - uneven liquidity");
    } else {
        info!("  ✓ Consistent bid sizes - uniform liquidity");
    }

    let size_ratio = bid_stats.max_level_size as f64 / bid_stats.min_level_size as f64;
    if size_ratio > 3.0 {
        info!(
            "  ⚠️  Large size ratio ({:.1}x) - some levels dominate",
            size_ratio
        );
    } else {
        info!("  ✓ Balanced level sizes (ratio: {:.1}x)", size_ratio);
    }
}

fn demo_market_pressure(book: &OrderBook) {
    info!("\n=== Market Pressure Analysis ===");

    let (buy_pressure, sell_pressure) = book.buy_sell_pressure();

    info!("Buy Pressure: {} units", buy_pressure);
    info!("Sell Pressure: {} units", sell_pressure);

    let total_pressure = buy_pressure + sell_pressure;
    let buy_pct = (buy_pressure as f64 / total_pressure as f64) * 100.0;
    let sell_pct = (sell_pressure as f64 / total_pressure as f64) * 100.0;

    info!("\nPressure Distribution:");
    info!("  Buy:  {:.1}%", buy_pct);
    info!("  Sell: {:.1}%", sell_pct);

    info!("\n💡 Market Sentiment:");
    let difference = buy_pressure as i64 - sell_pressure as i64;
    let diff_pct = (difference.abs() as f64 / total_pressure as f64) * 100.0;

    if diff_pct < 10.0 {
        info!("  → Balanced market ({:.1}% difference)", diff_pct);
        info!("  → Expect stable prices");
    } else if buy_pressure > sell_pressure {
        info!("  → Buy-heavy market ({:.1}% more buy volume)", diff_pct);
        info!("  → Potential upward pressure");
    } else {
        info!("  → Sell-heavy market ({:.1}% more sell volume)", diff_pct);
        info!("  → Potential downward pressure");
    }
}

fn demo_liquidity_health(book: &OrderBook) {
    info!("\n=== Liquidity Health Check ===");

    // Check different thresholds
    let thresholds = vec![
        (50, "Minimal"),
        (100, "Low"),
        (200, "Moderate"),
        (500, "High"),
    ];

    info!("\nLiquidity checks (top 5 levels):");
    for (threshold, label) in thresholds {
        let is_thin = book.is_thin_book(threshold, 5);
        let status = if is_thin { "❌ THIN" } else { "✓ OK" };
        info!("  {} threshold ({} units): {}", label, threshold, status);
    }

    // Detailed analysis
    let bid_stats = book.depth_statistics(Side::Buy, 5);
    let ask_stats = book.depth_statistics(Side::Sell, 5);

    info!("\n💡 Liquidity Assessment:");

    if bid_stats.total_volume < 100 || ask_stats.total_volume < 100 {
        info!("  ⚠️  WARNING: Thin order book detected!");
        info!("  → High slippage risk for large orders");
        info!("  → Consider splitting orders or waiting for better depth");
    } else if bid_stats.total_volume < 200 || ask_stats.total_volume < 200 {
        info!("  ⚠️  CAUTION: Moderate liquidity");
        info!("  → Use limit orders for better execution");
        info!("  → Monitor slippage carefully");
    } else {
        info!("  ✓ GOOD: Sufficient liquidity");
        info!("  → Market orders viable for reasonable sizes");
        info!("  → Low slippage expected");
    }

    // Check balance
    let imbalance = (bid_stats.total_volume as i64 - ask_stats.total_volume as i64).abs() as f64
        / (bid_stats.total_volume + ask_stats.total_volume) as f64;

    if imbalance > 0.3 {
        info!("  ⚠️  Liquidity imbalance: {:.1}%", imbalance * 100.0);
        if bid_stats.total_volume > ask_stats.total_volume {
            info!("  → More liquidity on bid side");
        } else {
            info!("  → More liquidity on ask side");
        }
    }
}

fn demo_depth_distribution(book: &OrderBook) {
    info!("\n=== Depth Distribution Analysis ===");

    // Analyze bid distribution
    info!("\n📊 Bid Side Distribution (5 bins):");
    let bid_distribution = book.depth_distribution(Side::Buy, 5);

    for (i, bin) in bid_distribution.iter().enumerate() {
        let bar_len = (bin.volume / 5).min(20) as usize;
        let bar = "█".repeat(bar_len);
        info!(
            "  Bin {}: ${}-{}: {} units [{}] ({} levels)",
            i + 1,
            bin.min_price,
            bin.max_price,
            bin.volume,
            bar,
            bin.level_count
        );
    }

    // Analyze ask distribution
    info!("\n📊 Ask Side Distribution (5 bins):");
    let ask_distribution = book.depth_distribution(Side::Sell, 5);

    for (i, bin) in ask_distribution.iter().enumerate() {
        let bar_len = (bin.volume / 5).min(20) as usize;
        let bar = "█".repeat(bar_len);
        info!(
            "  Bin {}: ${}-{}: {} units [{}] ({} levels)",
            i + 1,
            bin.min_price,
            bin.max_price,
            bin.volume,
            bar,
            bin.level_count
        );
    }

    // Analyze concentration
    info!("\n💡 Distribution Analysis:");

    let bid_total: u64 = bid_distribution.iter().map(|b| b.volume).sum();
    if let Some(max_bin) = bid_distribution.iter().max_by_key(|b| b.volume) {
        let concentration = (max_bin.volume as f64 / bid_total as f64) * 100.0;
        info!(
            "  Bid concentration: {:.1}% in bin ${}-{}",
            concentration, max_bin.min_price, max_bin.max_price
        );

        if concentration > 40.0 {
            info!("  ⚠️  High concentration - liquidity clustered");
        } else {
            info!("  ✓ Well-distributed liquidity");
        }
    }
}

fn demo_imbalance_detection(book: &OrderBook) {
    info!("\n=== Order Book Imbalance Detection ===");

    // Check imbalance at different depths
    let depths = vec![3, 5, 10];

    info!("\nImbalance at different depths:");
    for depth in depths {
        let imbalance = book.order_book_imbalance(depth);
        let direction = if imbalance > 0.0 { "BUY" } else { "SELL" };
        let strength = imbalance.abs();

        let indicator = if strength > 0.5 {
            "🔴 STRONG"
        } else if strength > 0.2 {
            "🟡 MODERATE"
        } else {
            "🟢 WEAK"
        };

        info!(
            "  Top {} levels: {:.3} ({} {} pressure)",
            depth, imbalance, indicator, direction
        );
    }

    // Detailed analysis
    let imbalance = book.order_book_imbalance(5);

    info!("\n💡 Imbalance Interpretation:");
    if imbalance.abs() < 0.1 {
        info!("  → Balanced market");
        info!("  → Expect range-bound trading");
        info!("  → Good for market making");
    } else if imbalance > 0.3 {
        info!("  → Strong buy pressure detected");
        info!("  → Potential bullish breakout");
        info!("  → Consider buying or staying long");
    } else if imbalance < -0.3 {
        info!("  → Strong sell pressure detected");
        info!("  → Potential bearish breakdown");
        info!("  → Consider selling or staying short");
    } else {
        info!("  → Mild imbalance");
        info!("  → Monitor for trend development");
        info!("  → Wait for confirmation");
    }
}

fn demo_trading_scenarios(book: &OrderBook) {
    info!("\n=== Practical Trading Scenarios ===");

    // Scenario 1: Order size decision
    info!("\n📈 Scenario 1: Determining Safe Order Size");

    let bid_stats = book.depth_statistics(Side::Buy, 5);
    let ask_stats = book.depth_statistics(Side::Sell, 5);

    let safe_buy_size = bid_stats.total_volume / 4; // 25% of depth
    let safe_sell_size = ask_stats.total_volume / 4;

    info!("  Available depth (top 5):");
    info!("    Buy side:  {} units", bid_stats.total_volume);
    info!("    Sell side: {} units", ask_stats.total_volume);
    info!("  Recommended max size (25% rule):");
    info!("    Buy orders:  {} units", safe_buy_size);
    info!("    Sell orders: {} units", safe_sell_size);

    // Scenario 2: Market condition assessment
    info!("\n📊 Scenario 2: Market Condition Assessment");

    let is_thin = book.is_thin_book(150, 5);
    let imbalance = book.order_book_imbalance(5);
    let (buy_pressure, sell_pressure) = book.buy_sell_pressure();

    info!(
        "  Liquidity: {}",
        if is_thin {
            "THIN ⚠️"
        } else {
            "ADEQUATE ✓"
        }
    );
    info!(
        "  Imbalance: {:.2} ({})",
        imbalance,
        if imbalance.abs() < 0.2 {
            "BALANCED ✓"
        } else {
            "SKEWED ⚠️"
        }
    );
    info!(
        "  Pressure ratio: {:.2} (buy/sell)",
        buy_pressure as f64 / sell_pressure as f64
    );

    info!("\n  Trading Recommendation:");
    if is_thin {
        info!("    → Use LIMIT ORDERS only");
        info!("    → Split large orders");
        info!("    → Monitor execution carefully");
    } else if imbalance.abs() > 0.3 {
        info!("    → Directional opportunity detected");
        if imbalance > 0.0 {
            info!("    → Consider BUYING (momentum trade)");
        } else {
            info!("    → Consider SELLING (momentum trade)");
        }
    } else {
        info!("    → Market making opportunity");
        info!("    → Place orders on both sides");
        info!("    → Capture spread");
    }

    // Scenario 3: Risk assessment
    info!("\n⚠️  Scenario 3: Risk Assessment");

    let bid_variability = bid_stats.std_dev_level_size / bid_stats.avg_level_size;
    let ask_variability = ask_stats.std_dev_level_size / ask_stats.avg_level_size;

    info!("  Level size variability:");
    info!("    Bid side: {:.2} (CV)", bid_variability);
    info!("    Ask side: {:.2} (CV)", ask_variability);

    if bid_variability > 0.5 || ask_variability > 0.5 {
        info!("\n  🔴 HIGH RISK:");
        info!("    → Uneven liquidity distribution");
        info!("    → Potential for sudden price moves");
        info!("    → Use wider stops");
        info!("    → Reduce position size");
    } else {
        info!("\n  🟢 MODERATE RISK:");
        info!("    → Consistent liquidity distribution");
        info!("    → Normal market conditions");
        info!("    → Standard risk management applies");
    }

    // Summary
    info!("\n✨ Key Takeaways:");
    info!("  1. Always check liquidity before trading");
    info!("  2. Monitor imbalance for directional signals");
    info!("  3. Adjust order size based on depth statistics");
    info!("  4. Use distribution analysis for risk assessment");
    info!("  5. Combine multiple indicators for better decisions");
}
