// examples/src/bin/intelligent_order_placement.rs
//
// This example demonstrates intelligent order placement utilities for market makers.
// These tools help traders:
// - Optimize queue position for maximum execution probability
// - Place orders at strategic price levels
// - Implement depth-based trading strategies
// - Understand competitive positioning in the order book
//
// Functions demonstrated:
// - `queue_ahead_at_price()`: Check queue position at a price level
// - `price_n_ticks_inside()`: Calculate price N ticks from best
// - `price_for_queue_position()`: Find price for target queue position
// - `price_at_depth_adjusted()`: Optimal price for depth-based strategies
//
// Run this example with:
//   cargo run --bin intelligent_order_placement
//   (from the examples directory)

use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() {
    // Set up logging
    setup_logger();
    info!("Intelligent Order Placement Example");

    // Create an order book with realistic depth
    let book = create_orderbook_with_depth("BTC/USD");

    // Display current book state
    display_orderbook_state(&book);

    // Demonstrate queue position analysis
    demo_queue_position_analysis(&book);

    // Demonstrate tick-based pricing
    demo_tick_based_pricing(&book);

    // Demonstrate queue position targeting
    demo_queue_position_targeting(&book);

    // Demonstrate depth-based strategies
    demo_depth_based_strategies(&book);

    // Practical use case: Market making strategy
    demo_market_making_strategy(&book);
}

fn create_orderbook_with_depth(symbol: &str) -> OrderBook {
    info!("\n=== Creating OrderBook with Market Depth ===");
    info!("Symbol: {}", symbol);

    let book = OrderBook::new(symbol);

    // Add buy orders (bids) - multiple orders at same price to show queue
    info!("\nAdding buy orders (bids):");

    // Best bid level with 3 orders
    let _ = book.add_limit_order(Id::new(), 50000, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 50000, 15, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 50000, 20, Side::Buy, TimeInForce::Gtc, None);
    info!("  @ 50000: 3 orders (10 + 15 + 20 = 45 total)");

    // Second level
    let _ = book.add_limit_order(Id::new(), 49950, 25, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 49950, 30, Side::Buy, TimeInForce::Gtc, None);
    info!("  @ 49950: 2 orders (25 + 30 = 55 total)");

    // Deeper levels
    let _ = book.add_limit_order(Id::new(), 49900, 35, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 49850, 40, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 49800, 45, Side::Buy, TimeInForce::Gtc, None);

    // Add sell orders (asks)
    info!("\nAdding sell orders (asks):");

    // Best ask level with 2 orders
    let _ = book.add_limit_order(Id::new(), 50100, 12, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 50100, 18, Side::Sell, TimeInForce::Gtc, None);
    info!("  @ 50100: 2 orders (12 + 18 = 30 total)");

    // Second level
    let _ = book.add_limit_order(Id::new(), 50150, 22, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 50150, 28, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 50150, 33, Side::Sell, TimeInForce::Gtc, None);
    info!("  @ 50150: 3 orders (22 + 28 + 33 = 83 total)");

    // Deeper levels
    let _ = book.add_limit_order(Id::new(), 50200, 38, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 50250, 43, Side::Sell, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new(), 50300, 48, Side::Sell, TimeInForce::Gtc, None);

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

fn demo_queue_position_analysis(book: &OrderBook) {
    info!("\n=== Queue Position Analysis ===");
    info!("Understanding your position in the order queue");

    // Analyze buy side
    info!("\nBuy side (bids):");
    let bid_prices = vec![50000, 49950, 49900];

    for price in bid_prices {
        let queue_depth = book.queue_ahead_at_price(price, Side::Buy);
        if queue_depth > 0 {
            info!("  @ {}: {} orders in queue", price, queue_depth);
            info!(
                "    → If you place an order here, you'll be {}th in line",
                queue_depth + 1
            );
        }
    }

    // Analyze sell side
    info!("\nSell side (asks):");
    let ask_prices = vec![50100, 50150, 50200];

    for price in ask_prices {
        let queue_depth = book.queue_ahead_at_price(price, Side::Sell);
        if queue_depth > 0 {
            info!("  @ {}: {} orders in queue", price, queue_depth);

            if queue_depth == 1 {
                info!("    → Light queue - good execution probability");
            } else if queue_depth <= 3 {
                info!("    → Moderate queue - decent execution chance");
            } else {
                info!("    → Heavy queue - consider alternative price");
            }
        }
    }
}

fn demo_tick_based_pricing(book: &OrderBook) {
    info!("\n=== Tick-Based Pricing ===");
    info!("Calculate prices relative to best bid/ask");

    let tick_size = 50; // $50 per tick

    // Buy side examples
    info!("\nBuy side (bids):");
    for n_ticks in 1..=3 {
        if let Some(price) = book.price_n_ticks_inside(n_ticks, tick_size, Side::Buy) {
            let best_bid = book.best_bid().unwrap();
            let distance = best_bid - price;
            info!(
                "  {} tick(s) inside: {} (${} below best bid)",
                n_ticks, price, distance
            );

            if n_ticks == 1 {
                info!("    → Competitive but not best - good for passive fills");
            } else if n_ticks == 2 {
                info!("    → More passive - lower execution probability");
            }
        }
    }

    // Sell side examples
    info!("\nSell side (asks):");
    for n_ticks in 1..=3 {
        if let Some(price) = book.price_n_ticks_inside(n_ticks, tick_size, Side::Sell) {
            let best_ask = book.best_ask().unwrap();
            let distance = price - best_ask;
            info!(
                "  {} tick(s) inside: {} (${} above best ask)",
                n_ticks, price, distance
            );
        }
    }

    // Practical application
    info!("\n💡 Practical Application:");
    if let Some(bid_price) = book.price_n_ticks_inside(1, tick_size, Side::Buy) {
        info!("  To be competitive without being at the touch:");
        info!("  → Place buy order at: {}", bid_price);
        info!("  → This gives you priority over deeper orders");
        info!("  → But saves you {} per unit vs best bid", tick_size);
    }
}

fn demo_queue_position_targeting(book: &OrderBook) {
    info!("\n=== Queue Position Targeting ===");
    info!("Find prices for specific queue positions");

    // Buy side
    info!("\nBuy side - finding prices for positions:");
    for position in 1..=5 {
        if let Some(price) = book.price_for_queue_position(position, Side::Buy) {
            let queue_depth = book.queue_ahead_at_price(price, Side::Buy);
            info!(
                "  Position {}: price = {} ({} orders at this level)",
                position, price, queue_depth
            );

            if position == 1 {
                info!("    ✓ Best bid - maximum execution probability");
            } else if position == 2 {
                info!("    • Second best - still competitive");
            }
        } else {
            info!("  Position {}: No price level exists", position);
            break;
        }
    }

    // Sell side
    info!("\nSell side - finding prices for positions:");
    for position in 1..=5 {
        if let Some(price) = book.price_for_queue_position(position, Side::Sell) {
            let queue_depth = book.queue_ahead_at_price(price, Side::Sell);
            info!(
                "  Position {}: price = {} ({} orders at this level)",
                position, price, queue_depth
            );
        } else {
            break;
        }
    }
}

fn demo_depth_based_strategies(book: &OrderBook) {
    info!("\n=== Depth-Based Strategies ===");
    info!("Optimize order placement based on cumulative depth");

    let tick_size = 50;

    // Buy side depth targets
    info!("\nBuy side depth-based pricing:");
    let buy_targets = vec![50, 100, 150];

    for target_depth in buy_targets {
        if let Some(price) = book.price_at_depth_adjusted(target_depth, tick_size, Side::Buy) {
            info!("\n  Target: {} units of depth", target_depth);
            info!("  Suggested price: {}", price);

            // Calculate actual depth at this price
            let actual_depth = calculate_depth_at_price(book, price, Side::Buy);
            info!("  Actual depth at price: {} units", actual_depth);

            if actual_depth >= target_depth {
                info!("  ✓ Your order will be just inside target depth");
            } else {
                info!("  ⚠ Insufficient depth - this is the deepest available");
            }
        }
    }

    // Sell side depth targets
    info!("\nSell side depth-based pricing:");
    let sell_targets = vec![30, 80, 120];

    for target_depth in sell_targets {
        if let Some(price) = book.price_at_depth_adjusted(target_depth, tick_size, Side::Sell) {
            info!("\n  Target: {} units of depth", target_depth);
            info!("  Suggested price: {}", price);

            let actual_depth = calculate_depth_at_price(book, price, Side::Sell);
            info!("  Actual depth at price: {} units", actual_depth);
        }
    }
}

fn demo_market_making_strategy(book: &OrderBook) {
    info!("\n=== Market Making Strategy Example ===");
    info!("Practical application of intelligent order placement");

    let tick_size = 50;

    info!("\nStrategy: Provide liquidity with optimized queue position");
    info!("Goal: Be competitive but not necessarily at touch");

    // Analyze current market
    let best_bid = book.best_bid().unwrap();
    let best_ask = book.best_ask().unwrap();
    let spread = best_ask - best_bid;

    info!("\nMarket Analysis:");
    info!("  Best Bid: {}", best_bid);
    info!("  Best Ask: {}", best_ask);
    info!(
        "  Spread: {} ({}%)",
        spread,
        (spread as f64 / best_bid as f64) * 100.0
    );

    // Check queue at best prices
    let bid_queue = book.queue_ahead_at_price(best_bid, Side::Buy);
    let ask_queue = book.queue_ahead_at_price(best_ask, Side::Sell);

    info!("\nQueue Analysis:");
    info!("  Orders at best bid: {}", bid_queue);
    info!("  Orders at best ask: {}", ask_queue);

    // Decision making
    info!("\n📊 Strategy Decision:");

    // Buy side decision
    if bid_queue > 3 {
        info!("\n  Buy side:");
        info!("  ✗ Best bid has heavy queue ({} orders)", bid_queue);

        if let Some(alt_price) = book.price_n_ticks_inside(1, tick_size, Side::Buy) {
            let alt_queue = book.queue_ahead_at_price(alt_price, Side::Buy);
            info!("  → Consider 1 tick inside at {}", alt_price);
            info!("  → Queue there: {} orders", alt_queue);
            info!("  → Cost: ${} per unit less competitive", tick_size);
            info!(
                "  ✓ RECOMMENDATION: Place at {} for better execution probability",
                alt_price
            );
        }
    } else {
        info!("\n  Buy side:");
        info!("  ✓ Light queue at best bid ({} orders)", bid_queue);
        info!("  ✓ RECOMMENDATION: Place at best bid {}", best_bid);
    }

    // Sell side decision
    if ask_queue > 3 {
        info!("\n  Sell side:");
        info!("  ✗ Best ask has heavy queue ({} orders)", ask_queue);

        if let Some(alt_price) = book.price_n_ticks_inside(1, tick_size, Side::Sell) {
            let alt_queue = book.queue_ahead_at_price(alt_price, Side::Sell);
            info!("  → Consider 1 tick inside at {}", alt_price);
            info!("  → Queue there: {} orders", alt_queue);
            info!("  ✓ RECOMMENDATION: Place at {}", alt_price);
        }
    } else {
        info!("\n  Sell side:");
        info!("  ✓ Light queue at best ask ({} orders)", ask_queue);
        info!("  ✓ RECOMMENDATION: Place at best ask {}", best_ask);
    }

    // Alternative: Depth-based strategy
    info!("\n💡 Alternative: Depth-Based Approach");
    let target_depth = 100;

    if let Some(bid_price) = book.price_at_depth_adjusted(target_depth, tick_size, Side::Buy) {
        info!(
            "  Buy: Place at {} to be just inside {} units depth",
            bid_price, target_depth
        );
    }

    if let Some(ask_price) = book.price_at_depth_adjusted(target_depth, tick_size, Side::Sell) {
        info!(
            "  Sell: Place at {} to be just inside {} units depth",
            ask_price, target_depth
        );
    }

    info!("\n✨ Key Takeaways:");
    info!("  1. Queue position matters - heavy queues = lower execution probability");
    info!("  2. Sometimes 1 tick worse price = much better execution");
    info!("  3. Depth-based strategies can optimize risk/reward");
    info!("  4. Monitor and adjust based on market dynamics");
}

// Helper function to calculate depth at a specific price
fn calculate_depth_at_price(book: &OrderBook, target_price: u128, side: Side) -> u64 {
    let best_price = match side {
        Side::Buy => book.best_bid(),
        Side::Sell => book.best_ask(),
    };

    if let Some(best) = best_price {
        match side {
            Side::Buy => book.liquidity_in_range(target_price, best, side),
            Side::Sell => book.liquidity_in_range(best, target_price, side),
        }
    } else {
        0
    }
}
