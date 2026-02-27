// examples/src/bin/functional_iterators.rs
//
// This example demonstrates functional-style iterators for order book analysis.
// These iterators provide memory-efficient, composable ways to analyze market depth
// without unnecessary allocations, supporting lazy evaluation and early short-circuiting.
//
// Functions demonstrated:
// - `levels_with_cumulative_depth()`: Iterate with running depth totals
// - `levels_until_depth()`: Stop automatically at target depth
// - `levels_in_range()`: Filter by price range
// - `find_level()`: Find first matching level
//
// Run this example with:
//   cargo run --bin functional_iterators
//   (from the examples directory)

use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() {
    // Set up logging
    setup_logger();
    info!("Functional Iterators Example");

    // Create an order book with realistic depth
    let book = create_orderbook_with_depth("BTC/USD");

    // Display current book state
    display_orderbook_state(&book);

    // Demonstrate basic iteration
    demo_basic_iteration(&book);

    // Demonstrate lazy evaluation benefits
    demo_lazy_evaluation(&book);

    // Demonstrate iterator composition
    demo_iterator_composition(&book);

    // Demonstrate range-based analysis
    demo_range_analysis(&book);

    // Demonstrate find operations
    demo_find_operations(&book);

    // Practical use cases
    demo_practical_use_cases(&book);
}

fn create_orderbook_with_depth(symbol: &str) -> OrderBook {
    info!("\n=== Creating OrderBook with Depth ===");
    info!("Symbol: {}", symbol);

    let book = OrderBook::new(symbol);

    // Add buy orders (bids) with varying sizes
    info!("\nAdding buy orders (bids):");
    let bid_orders = vec![
        (50000, 5),
        (49950, 10),
        (49900, 15),
        (49850, 20),
        (49800, 25),
        (49750, 30),
        (49700, 35),
        (49650, 40),
        (49600, 45),
        (49550, 50),
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
        info!("  Bid: {} @ {}", quantity, price);
    }

    // Add sell orders (asks) with varying sizes
    info!("\nAdding sell orders (asks):");
    let ask_orders = vec![
        (50100, 8),
        (50150, 12),
        (50200, 16),
        (50250, 20),
        (50300, 24),
        (50350, 28),
        (50400, 32),
        (50450, 36),
        (50500, 40),
        (50550, 44),
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

fn demo_basic_iteration(book: &OrderBook) {
    info!("\n=== Basic Iteration ===");
    info!("Iterate through levels with cumulative depth tracking");

    info!("\nBuy side (first 5 levels):");
    for (i, level) in book
        .levels_with_cumulative_depth(Side::Buy)
        .take(5)
        .enumerate()
    {
        info!(
            "  Level {}: Price={}, Qty={}, Cumulative={}",
            i + 1,
            level.price,
            level.quantity,
            level.cumulative_depth
        );
    }

    info!("\nSell side (first 5 levels):");
    for (i, level) in book
        .levels_with_cumulative_depth(Side::Sell)
        .take(5)
        .enumerate()
    {
        info!(
            "  Level {}: Price={}, Qty={}, Cumulative={}",
            i + 1,
            level.price,
            level.quantity,
            level.cumulative_depth
        );
    }
}

fn demo_lazy_evaluation(book: &OrderBook) {
    info!("\n=== Lazy Evaluation Benefits ===");
    info!("Iterators don't allocate memory upfront");

    // Example 1: Early termination
    info!("\n1. Find first level with >20 units (short-circuits early):");
    for level in book.levels_with_cumulative_depth(Side::Buy) {
        if level.quantity > 20 {
            info!("  Found: {} units @ {}", level.quantity, level.price);
            info!("  Stopped early without processing remaining levels");
            break;
        }
    }

    // Example 2: Automatic depth limit
    info!("\n2. Process only until 100 units accumulated:");
    let count = book.levels_until_depth(100, Side::Buy).count();
    info!("  Processed {} levels to reach 100 units", count);
    info!("  Remaining levels were never touched (efficient!)");

    // Example 3: No vector allocation
    info!("\n3. Calculate total without allocating vectors:");
    let total: u64 = book
        .levels_with_cumulative_depth(Side::Buy)
        .take(5)
        .map(|level| level.quantity)
        .sum();
    info!(
        "  Total quantity in top 5 levels: {} (no intermediate allocations)",
        total
    );
}

fn demo_iterator_composition(book: &OrderBook) {
    info!("\n=== Iterator Composition ===");
    info!("Chain multiple operations elegantly");

    // Example 1: Filter and map
    info!("\n1. Filter levels with >15 units, sum their quantities:");
    let total: u64 = book
        .levels_with_cumulative_depth(Side::Buy)
        .filter(|level| level.quantity > 15)
        .map(|level| level.quantity)
        .sum();
    info!("  Total of large orders: {}", total);

    // Example 2: Take while condition
    info!("\n2. Take levels while cumulative depth < 50:");
    let levels: Vec<_> = book
        .levels_with_cumulative_depth(Side::Buy)
        .take_while(|level| level.cumulative_depth < 50)
        .collect();
    info!(
        "  Collected {} levels before reaching 50 units",
        levels.len()
    );

    // Example 3: Complex pipeline
    info!("\n3. Complex analysis pipeline:");
    let avg = book
        .levels_with_cumulative_depth(Side::Buy)
        .take(5)
        .filter(|level| level.quantity >= 10)
        .map(|level| level.quantity as f64)
        .sum::<f64>()
        / 5.0;
    info!("  Average size of top 5 levels (>=10 units): {:.2}", avg);

    // Example 4: Find and enumerate
    info!("\n4. Enumerate and find specific condition:");
    if let Some((idx, level)) = book
        .levels_with_cumulative_depth(Side::Sell)
        .enumerate()
        .find(|(_, level)| level.cumulative_depth > 30)
    {
        info!(
            "  First level with cumulative >30: index={}, price={}",
            idx, level.price
        );
    }
}

fn demo_range_analysis(book: &OrderBook) {
    info!("\n=== Range-Based Analysis ===");
    info!("Analyze specific price bands efficiently");

    // Example 1: Total liquidity in range
    info!("\n1. Total liquidity in price range:");
    let ranges = vec![
        (49800, 50000, "Near touch"),
        (49500, 49800, "Mid depth"),
        (49000, 49500, "Deep book"),
    ];

    for (min, max, desc) in ranges {
        let total: u64 = book
            .levels_in_range(min, max, Side::Buy)
            .map(|level| level.quantity)
            .sum();
        info!("  {}: {} units ({}-{})", desc, total, min, max);
    }

    // Example 2: Level count in range
    info!("\n2. Count levels in ranges:");
    let count_near = book.levels_in_range(49900, 50000, Side::Buy).count();
    let count_mid = book.levels_in_range(49700, 49900, Side::Buy).count();
    info!("  Near touch (49900-50000): {} levels", count_near);
    info!("  Mid depth (49700-49900): {} levels", count_mid);

    // Example 3: Average size in range
    info!("\n3. Average order size in range:");
    let levels: Vec<_> = book.levels_in_range(49700, 50000, Side::Buy).collect();
    if !levels.is_empty() {
        let total: u64 = levels.iter().map(|l| l.quantity).sum();
        let avg = total as f64 / levels.len() as f64;
        info!("  Range 49700-50000: {:.2} units avg", avg);
    }
}

fn demo_find_operations(book: &OrderBook) {
    info!("\n=== Find Operations ===");
    info!("Search with custom predicates");

    // Example 1: Find by quantity threshold
    info!("\n1. Find first level with quantity > 25:");
    if let Some(level) = book.find_level(Side::Buy, |info| info.quantity > 25) {
        info!("  Found: {} units @ {}", level.quantity, level.price);
    }

    // Example 2: Find by cumulative depth
    info!("\n2. Find where cumulative depth reaches 100:");
    if let Some(level) = book.find_level(Side::Buy, |info| info.cumulative_depth >= 100) {
        info!(
            "  Reached @ price={}, cumulative={}",
            level.price, level.cumulative_depth
        );
    }

    // Example 3: Find by price condition
    info!("\n3. Find first level below 49800:");
    if let Some(level) = book.find_level(Side::Buy, |info| info.price < 49800) {
        info!(
            "  Found: price={}, quantity={}",
            level.price, level.quantity
        );
    }

    // Example 4: Complex predicate
    info!("\n4. Find level with quantity >20 AND cumulative >50:");
    if let Some(level) = book.find_level(Side::Buy, |info| {
        info.quantity > 20 && info.cumulative_depth > 50
    }) {
        info!(
            "  Found: qty={}, cumulative={}, @ {}",
            level.quantity, level.cumulative_depth, level.price
        );
    }
}

fn demo_practical_use_cases(book: &OrderBook) {
    info!("\n=== Practical Use Cases ===");

    // Use case 1: Execution planning
    info!("\n1. Execution Planning:");
    info!("   How many levels needed to fill 150 units?");
    let levels: Vec<_> = book.levels_until_depth(150, Side::Buy).collect();
    info!("   → Need to consume {} price levels", levels.len());
    if let Some(last) = levels.last() {
        info!("   → Worst execution price: {}", last.price);
        info!("   → Total available: {} units", last.cumulative_depth);
    }

    // Use case 2: Liquidity analysis
    info!("\n2. Liquidity Distribution:");
    let ranges = vec![
        (0, 25, "Top 25 units"),
        (25, 50, "Next 25 units"),
        (50, 100, "Next 50 units"),
    ];

    for (start, end, desc) in ranges {
        let levels: Vec<_> = book
            .levels_with_cumulative_depth(Side::Buy)
            .skip_while(|l| l.cumulative_depth <= start)
            .take_while(|l| l.cumulative_depth <= end)
            .collect();

        if !levels.is_empty() {
            let avg_price =
                levels.iter().map(|l| l.price).sum::<u128>() as f64 / levels.len() as f64;
            info!(
                "   {}: avg price {:.0}, {} levels",
                desc,
                avg_price,
                levels.len()
            );
        }
    }

    // Use case 3: Risk assessment
    info!("\n3. Risk Assessment:");
    info!("   Analyze slippage for different order sizes:");
    let sizes = vec![25, 50, 100, 200];

    for size in sizes {
        if let Some(last_level) = book.levels_until_depth(size, Side::Buy).last() {
            let best_bid = book.best_bid().unwrap();
            let slippage = best_bid - last_level.price;
            let slippage_pct = (slippage as f64 / best_bid as f64) * 100.0;

            info!(
                "   Order size {}: worst price={}, slippage={:.3}%",
                size, last_level.price, slippage_pct
            );
        }
    }

    // Use case 4: Market quality metrics
    info!("\n4. Market Quality Metrics:");

    // Depth at different levels
    let depth_at_5 = book
        .levels_with_cumulative_depth(Side::Buy)
        .nth(4)
        .map(|l| l.cumulative_depth)
        .unwrap_or(0);

    info!("   Depth at 5th level: {} units", depth_at_5);

    // Average spread between levels
    let levels: Vec<_> = book
        .levels_with_cumulative_depth(Side::Buy)
        .take(5)
        .collect();

    if levels.len() >= 2 {
        let spreads: Vec<u128> = levels.windows(2).map(|w| w[0].price - w[1].price).collect();
        let avg_spread = spreads.iter().sum::<u128>() as f64 / spreads.len() as f64;
        info!("   Average spread between top 5 levels: {:.0}", avg_spread);
    }

    // Use case 5: Smart order routing
    info!("\n5. Smart Order Routing Decision:");
    let target_size = 75;

    // Check if we can fill without excessive slippage
    if let Some(last_level) = book.levels_until_depth(target_size, Side::Buy).last() {
        let best = book.best_bid().unwrap();
        let slippage_bps = ((best - last_level.price) as f64 / best as f64) * 10000.0;

        info!("   Target order: {} units", target_size);
        info!("   Expected slippage: {:.2} bps", slippage_bps);

        if slippage_bps < 10.0 {
            info!("   ✓ RECOMMENDATION: Execute on this venue (low slippage)");
        } else if slippage_bps < 50.0 {
            info!("   • RECOMMENDATION: Acceptable, but compare other venues");
        } else {
            info!("   ✗ RECOMMENDATION: Split order or use different venue");
        }
    }

    info!("\n✨ Key Benefits of Functional Iterators:");
    info!("  • Zero allocation - no intermediate vectors");
    info!("  • Lazy evaluation - compute only what's needed");
    info!("  • Composable - chain operations elegantly");
    info!("  • Short-circuit - stop early when condition met");
    info!("  • Expressive - readable functional style");
}
