// examples/src/bin/depth_analysis.rs
//
// This example demonstrates the depth analysis methods available in the OrderBook.
// These methods are useful for:
// - Market making strategies
// - Liquidity analysis
// - Market impact estimation
// - Order execution planning
//
// Methods demonstrated:
// - `price_at_depth()`: Find the price level where cumulative depth reaches a target
// - `cumulative_depth_to_target()`: Get both price and actual cumulative depth
// - `total_depth_at_levels()`: Calculate total depth in the first N price levels
//
// Run this example with:
//   cargo run --bin depth_analysis
//   (from the examples directory)

use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() {
    // Set up logging
    setup_logger();
    info!("Depth Analysis Example");

    // Create a new order book for a symbol
    let book = create_orderbook_with_liquidity("ETH/USD");

    // Display current book state
    display_orderbook_state(&book);

    // Demonstrate price_at_depth method
    demo_price_at_depth(&book);

    // Demonstrate cumulative_depth_to_target method
    demo_cumulative_depth_to_target(&book);

    // Demonstrate total_depth_at_levels method
    demo_total_depth_at_levels(&book);

    // Demonstrate practical use case: market impact estimation
    demo_market_impact_estimation(&book);
}

fn create_orderbook_with_liquidity(symbol: &str) -> OrderBook {
    info!("\n=== Creating OrderBook with Liquidity ===");
    info!("Symbol: {}", symbol);

    let book = OrderBook::new(symbol);

    // Add buy orders (bids) at different price levels
    info!("\nAdding buy orders (bids):");
    let bid_orders = vec![
        (2000, 10), // Price: 2000, Quantity: 10
        (1999, 15),
        (1998, 20),
        (1997, 25),
        (1996, 30),
        (1995, 35),
    ];

    for (price, quantity) in bid_orders {
        let order_id = Id::new();
        let _ = book.add_limit_order(order_id, price, quantity, Side::Buy, TimeInForce::Gtc, None);
        info!(
            "  Added bid: {} @ {} (total: {})",
            quantity, price, quantity
        );
    }

    // Add sell orders (asks) at different price levels
    info!("\nAdding sell orders (asks):");
    let ask_orders = vec![
        (2001, 12), // Price: 2001, Quantity: 12
        (2002, 18),
        (2003, 22),
        (2004, 28),
        (2005, 32),
        (2006, 38),
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
        info!(
            "  Added ask: {} @ {} (total: {})",
            quantity, price, quantity
        );
    }

    book
}

fn display_orderbook_state(book: &OrderBook) {
    info!("\n=== OrderBook State ===");

    // Display best bid and ask
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

fn demo_price_at_depth(book: &OrderBook) {
    info!("\n=== price_at_depth() Demo ===");
    info!("Finds the price level where cumulative depth reaches a target quantity");

    // Test on buy side
    info!("\nBuy side (bids - highest to lowest):");
    let buy_targets = vec![10, 25, 50, 100, 200];

    for target in buy_targets {
        match book.price_at_depth(target, Side::Buy) {
            Some(price) => info!("  Target depth {}: reached at price {}", target, price),
            None => info!("  Target depth {}: insufficient liquidity", target),
        }
    }

    // Test on sell side
    info!("\nSell side (asks - lowest to highest):");
    let sell_targets = vec![12, 30, 60, 120, 200];

    for target in sell_targets {
        match book.price_at_depth(target, Side::Sell) {
            Some(price) => info!("  Target depth {}: reached at price {}", target, price),
            None => info!("  Target depth {}: insufficient liquidity", target),
        }
    }
}

fn demo_cumulative_depth_to_target(book: &OrderBook) {
    info!("\n=== cumulative_depth_to_target() Demo ===");
    info!("Returns both the price and actual cumulative depth at target");

    // Test on buy side
    info!("\nBuy side (bids):");
    let buy_targets = vec![10, 25, 50, 100];

    for target in buy_targets {
        match book.cumulative_depth_to_target(target, Side::Buy) {
            Some((price, actual_depth)) => {
                info!(
                    "  Target {}: price={}, actual_depth={}",
                    target, price, actual_depth
                );
                if actual_depth > target {
                    info!(
                        "    (Cumulative depth exceeded target by {})",
                        actual_depth - target
                    );
                }
            }
            None => info!("  Target {}: insufficient liquidity", target),
        }
    }

    // Test on sell side
    info!("\nSell side (asks):");
    let sell_targets = vec![12, 30, 60, 120];

    for target in sell_targets {
        match book.cumulative_depth_to_target(target, Side::Sell) {
            Some((price, actual_depth)) => {
                info!(
                    "  Target {}: price={}, actual_depth={}",
                    target, price, actual_depth
                );
                if actual_depth > target {
                    info!(
                        "    (Cumulative depth exceeded target by {})",
                        actual_depth - target
                    );
                }
            }
            None => info!("  Target {}: insufficient liquidity", target),
        }
    }
}

fn demo_total_depth_at_levels(book: &OrderBook) {
    info!("\n=== total_depth_at_levels() Demo ===");
    info!("Calculates total depth available in the first N price levels");

    // Test on buy side
    info!("\nBuy side (bids):");
    for levels in 1..=6 {
        let depth = book.total_depth_at_levels(levels, Side::Buy);
        info!("  Top {} level(s): total depth = {}", levels, depth);
    }

    // Test on sell side
    info!("\nSell side (asks):");
    for levels in 1..=6 {
        let depth = book.total_depth_at_levels(levels, Side::Sell);
        info!("  Top {} level(s): total depth = {}", levels, depth);
    }

    // Test edge cases
    info!("\nEdge cases:");
    let zero_depth = book.total_depth_at_levels(0, Side::Buy);
    info!("  Zero levels: {}", zero_depth);

    let excessive_depth = book.total_depth_at_levels(100, Side::Buy);
    info!(
        "  Excessive levels (100): {} (returns all available)",
        excessive_depth
    );
}

fn demo_market_impact_estimation(book: &OrderBook) {
    info!("\n=== Practical Use Case: Market Impact Estimation ===");
    info!("Estimating the impact of executing large market orders");

    // Simulate large buy market order
    let buy_quantity = 80;
    info!("\nSimulating market BUY order of {} units:", buy_quantity);

    if let Some((worst_price, total_filled)) =
        book.cumulative_depth_to_target(buy_quantity, Side::Sell)
    {
        if let Some(best_price) = book.best_ask() {
            let price_impact = worst_price.saturating_sub(best_price);
            let price_impact_pct = (price_impact as f64 / best_price as f64) * 100.0;

            info!("  Best ask price: {}", best_price);
            info!("  Worst fill price: {}", worst_price);
            info!(
                "  Price impact: {} ({:.2}%)",
                price_impact, price_impact_pct
            );
            info!("  Total filled: {}", total_filled);

            if total_filled >= buy_quantity {
                info!("  ✓ Order can be fully filled");
            } else {
                info!(
                    "  ✗ Insufficient liquidity (only {} available)",
                    total_filled
                );
            }

            // Calculate average fill price (simplified)
            let avg_price = (best_price + worst_price) / 2;
            info!("  Estimated avg fill price: ~{}", avg_price);
        }
    } else {
        info!("  ✗ Insufficient liquidity to fill order");
    }

    // Simulate large sell market order
    let sell_quantity = 70;
    info!("\nSimulating market SELL order of {} units:", sell_quantity);

    if let Some((worst_price, total_filled)) =
        book.cumulative_depth_to_target(sell_quantity, Side::Buy)
    {
        if let Some(best_price) = book.best_bid() {
            let price_impact = best_price.saturating_sub(worst_price);
            let price_impact_pct = (price_impact as f64 / best_price as f64) * 100.0;

            info!("  Best bid price: {}", best_price);
            info!("  Worst fill price: {}", worst_price);
            info!(
                "  Price impact: {} ({:.2}%)",
                price_impact, price_impact_pct
            );
            info!("  Total filled: {}", total_filled);

            if total_filled >= sell_quantity {
                info!("  ✓ Order can be fully filled");
            } else {
                info!(
                    "  ✗ Insufficient liquidity (only {} available)",
                    total_filled
                );
            }

            // Calculate average fill price (simplified)
            let avg_price = (best_price + worst_price) / 2;
            info!("  Estimated avg fill price: ~{}", avg_price);
        }
    } else {
        info!("  ✗ Insufficient liquidity to fill order");
    }

    // Market depth analysis
    info!("\n=== Market Depth Analysis ===");
    info!("Analyzing liquidity distribution across price levels:");

    info!("\nBid side depth distribution:");
    for level in 1..=5 {
        let depth = book.total_depth_at_levels(level, Side::Buy);
        let prev_depth = if level > 1 {
            book.total_depth_at_levels(level - 1, Side::Buy)
        } else {
            0
        };
        let level_depth = depth - prev_depth;
        info!(
            "  Level {}: {} units (cumulative: {})",
            level, level_depth, depth
        );
    }

    info!("\nAsk side depth distribution:");
    for level in 1..=5 {
        let depth = book.total_depth_at_levels(level, Side::Sell);
        let prev_depth = if level > 1 {
            book.total_depth_at_levels(level - 1, Side::Sell)
        } else {
            0
        };
        let level_depth = depth - prev_depth;
        info!(
            "  Level {}: {} units (cumulative: {})",
            level, level_depth, depth
        );
    }
}
