// examples/src/bin/market_metrics.rs
//
// This example demonstrates the market metrics methods available in the OrderBook.
// These metrics are essential for:
// - Market making strategies
// - Risk management
// - Trading signal generation
// - Order execution optimization
//
// Metrics demonstrated:
// - `mid_price()`: Average of best bid and ask
// - `spread_absolute()`: Absolute spread (ask - bid)
// - `spread_bps()`: Spread in basis points
// - `vwap()`: Volume-Weighted Average Price
// - `micro_price()`: Weighted price by volume at best levels
// - `order_book_imbalance()`: Buy/sell pressure indicator
//
// Run this example with:
//   cargo run --bin market_metrics
//   (from the examples directory)

use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() {
    // Set up logging
    setup_logger();
    info!("Market Metrics Example");

    // Create a realistic order book with liquidity
    let book = create_orderbook_with_liquidity("ETH/USD");

    // Display current book state
    display_orderbook_state(&book);

    // Demonstrate basic price metrics
    demo_price_metrics(&book);

    // Demonstrate spread metrics
    demo_spread_metrics(&book);

    // Demonstrate VWAP calculations
    demo_vwap_calculations(&book);

    // Demonstrate micro price
    demo_micro_price(&book);

    // Demonstrate order book imbalance
    demo_order_book_imbalance(&book);

    // Practical use case: trading signals
    demo_trading_signals(&book);
}

fn create_orderbook_with_liquidity(symbol: &str) -> OrderBook {
    info!("\n=== Creating OrderBook with Realistic Liquidity ===");
    info!("Symbol: {}", symbol);

    let book = OrderBook::new(symbol);

    // Add buy orders (bids) at different price levels
    info!("\nAdding buy orders (bids):");
    let bid_orders = vec![
        (3000, 100), // Best bid
        (2995, 150),
        (2990, 200),
        (2985, 250),
        (2980, 300),
    ];

    for (price, quantity) in bid_orders {
        let order_id = Id::new();
        let _ = book.add_limit_order(order_id, price, quantity, Side::Buy, TimeInForce::Gtc, None);
        info!("  Bid: {} @ {}", quantity, price);
    }

    // Add sell orders (asks) at different price levels
    info!("\nAdding sell orders (asks):");
    let ask_orders = vec![
        (3010, 120), // Best ask
        (3015, 180),
        (3020, 220),
        (3025, 280),
        (3030, 320),
    ];

    for (price, quantity) in ask_orders {
        let order_id = Id::new();
        let _ = book.add_limit_order(
            order_id,
            price,
            quantity,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
        info!("  Ask: {} @ {}", quantity, price);
    }

    book
}

fn display_orderbook_state(book: &OrderBook) {
    info!("\n=== OrderBook State ===");

    if let Some(best_bid) = book.best_bid() {
        info!("Best Bid: {}", best_bid);
    }

    if let Some(best_ask) = book.best_ask() {
        info!("Best Ask: {}", best_ask);
    }

    if let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) {
        let spread = ask.saturating_sub(bid);
        info!("Spread: {}", spread);
    }
}

fn demo_price_metrics(book: &OrderBook) {
    info!("\n=== Price Metrics ===");
    info!("Basic price calculations from the order book");

    if let Some(mid) = book.mid_price() {
        info!("Mid Price: {:.2}", mid);
        info!("  Formula: (best_bid + best_ask) / 2");
    }

    if let Some(last_trade) = book.last_trade_price() {
        info!("Last Trade Price: {}", last_trade);
    } else {
        info!("Last Trade Price: None (no trades yet)");
    }
}

fn demo_spread_metrics(book: &OrderBook) {
    info!("\n=== Spread Metrics ===");
    info!("Measuring the cost of immediate execution");

    if let Some(spread_abs) = book.spread_absolute() {
        info!("Absolute Spread: {} price units", spread_abs);
        info!("  This is the difference between best ask and best bid");
    }

    if let Some(spread_bps) = book.spread_bps(None) {
        info!("Spread in Basis Points: {:.2} bps", spread_bps);
        info!("  Formula: ((ask - bid) / mid_price) * 10,000");
        info!("  1 basis point = 0.01%");

        if spread_bps < 5.0 {
            info!("  ✓ Tight spread - highly liquid market");
        } else if spread_bps < 20.0 {
            info!("  • Normal spread - decent liquidity");
        } else {
            info!("  ⚠ Wide spread - low liquidity");
        }
    }

    // Demonstrate custom multiplier (percentage)
    if let Some(spread_pct) = book.spread_bps(Some(100.0)) {
        info!("\nSpread as Percentage: {:.4}%", spread_pct);
        info!("  Using custom multiplier of 100");
    }
}

fn demo_vwap_calculations(book: &OrderBook) {
    info!("\n=== VWAP (Volume-Weighted Average Price) ===");
    info!("Calculating execution price for different order sizes");

    // Test different buy order sizes
    info!("\nBuying scenarios (executing against asks):");
    let buy_quantities = vec![100, 200, 400];

    for quantity in buy_quantities {
        match book.vwap(quantity, Side::Buy) {
            Some(vwap) => {
                if let Some(best_ask) = book.best_ask() {
                    let slippage = vwap - best_ask as f64;
                    let slippage_bps = (slippage / best_ask as f64) * 10_000.0;
                    info!("  Buy {} units:", quantity);
                    info!("    VWAP: {:.2}", vwap);
                    info!("    Slippage: {:.2} ({:.2} bps)", slippage, slippage_bps);
                }
            }
            None => info!("  Buy {} units: Insufficient liquidity", quantity),
        }
    }

    // Test different sell order sizes
    info!("\nSelling scenarios (executing against bids):");
    let sell_quantities = vec![100, 200, 400];

    for quantity in sell_quantities {
        match book.vwap(quantity, Side::Sell) {
            Some(vwap) => {
                if let Some(best_bid) = book.best_bid() {
                    let slippage = best_bid as f64 - vwap;
                    let slippage_bps = (slippage / best_bid as f64) * 10_000.0;
                    info!("  Sell {} units:", quantity);
                    info!("    VWAP: {:.2}", vwap);
                    info!("    Slippage: {:.2} ({:.2} bps)", slippage, slippage_bps);
                }
            }
            None => info!("  Sell {} units: Insufficient liquidity", quantity),
        }
    }
}

fn demo_micro_price(book: &OrderBook) {
    info!("\n=== Micro Price ===");
    info!("Volume-weighted price at best bid/ask levels");

    if let (Some(mid), Some(micro)) = (book.mid_price(), book.micro_price()) {
        info!("Mid Price:   {:.2}", mid);
        info!("Micro Price: {:.2}", micro);

        let diff = micro - mid;
        if diff.abs() < 0.01 {
            info!("  • Balanced market - similar volumes on both sides");
        } else if diff > 0.0 {
            info!("  • Micro price > Mid price");
            info!("    More volume on bid side (buying pressure)");
        } else {
            info!("  • Micro price < Mid price");
            info!("    More volume on ask side (selling pressure)");
        }
    }
}

fn demo_order_book_imbalance(book: &OrderBook) {
    info!("\n=== Order Book Imbalance ===");
    info!("Measuring buy/sell pressure across price levels");

    let level_counts = vec![1, 3, 5];

    for levels in level_counts {
        let imbalance = book.order_book_imbalance(levels);
        let bid_vol = book.total_depth_at_levels(levels, Side::Buy);
        let ask_vol = book.total_depth_at_levels(levels, Side::Sell);

        info!("\nTop {} level(s):", levels);
        info!("  Bid volume: {}", bid_vol);
        info!("  Ask volume: {}", ask_vol);
        info!("  Imbalance: {:.3}", imbalance);

        if imbalance > 0.2 {
            info!("  ↗ Strong buy pressure");
        } else if imbalance > 0.05 {
            info!("  ↗ Moderate buy pressure");
        } else if imbalance < -0.2 {
            info!("  ↘ Strong sell pressure");
        } else if imbalance < -0.05 {
            info!("  ↘ Moderate sell pressure");
        } else {
            info!("  → Balanced market");
        }
    }
}

fn demo_trading_signals(book: &OrderBook) {
    info!("\n=== Trading Signals Generation ===");
    info!("Using metrics to generate actionable signals");

    let spread_bps = book.spread_bps(None).unwrap_or(f64::MAX);
    let imbalance = book.order_book_imbalance(3);
    let micro = book.micro_price().unwrap_or(0.0);
    let mid = book.mid_price().unwrap_or(0.0);

    info!("\nSignal Analysis:");

    // Liquidity signal
    info!("\n1. Liquidity Signal:");
    if spread_bps < 10.0 {
        info!("   ✓ Good liquidity (spread < 10 bps)");
        info!("   → Safe for large orders");
    } else {
        info!("   ⚠ Poor liquidity (spread >= 10 bps)");
        info!("   → Use limit orders to avoid slippage");
    }

    // Directional signal
    info!("\n2. Directional Signal:");
    if imbalance > 0.15 {
        info!("   ↗ Bullish imbalance ({:.2})", imbalance);
        info!("   → Consider buying on dips");
    } else if imbalance < -0.15 {
        info!("   ↘ Bearish imbalance ({:.2})", imbalance);
        info!("   → Consider selling rallies");
    } else {
        info!("   → Neutral ({:.2})", imbalance);
        info!("   → Wait for clearer signal");
    }

    // Fair value signal
    info!("\n3. Fair Value Signal:");
    let micro_diff_bps = ((micro - mid) / mid) * 10_000.0;
    info!("   Mid:   {:.2}", mid);
    info!("   Micro: {:.2} ({:+.2} bps)", micro, micro_diff_bps);

    if micro_diff_bps.abs() < 1.0 {
        info!("   → Fair value close to mid price");
    } else if micro_diff_bps > 0.0 {
        info!("   → Buying pressure pushing price up");
    } else {
        info!("   → Selling pressure pushing price down");
    }

    // VWAP execution recommendation
    info!("\n4. Execution Recommendation:");
    if let Some(vwap_100) = book.vwap(100, Side::Buy) {
        let best_ask = book.best_ask().unwrap() as f64;
        let slippage_bps = ((vwap_100 - best_ask) / best_ask) * 10_000.0;

        info!("   For 100 unit buy order:");
        info!("   Best ask: {:.2}", best_ask);
        info!("   Expected VWAP: {:.2}", vwap_100);
        info!("   Expected slippage: {:.2} bps", slippage_bps);

        if slippage_bps < 5.0 {
            info!("   ✓ Low slippage - market order OK");
        } else {
            info!("   ⚠ High slippage - consider limit order");
        }
    }
}
