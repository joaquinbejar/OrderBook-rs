// examples/src/bin/market_impact_simulation.rs
//
// This example demonstrates market impact simulation and liquidity analysis.
// These tools are essential for:
// - Pre-trade analysis and risk management
// - Smart order routing decisions
// - Optimal order sizing
// - Execution cost estimation
//
// Functions demonstrated:
// - `market_impact()`: Analyze the impact of an order before execution
// - `simulate_market_order()`: Step-by-step execution simulation
// - `liquidity_in_range()`: Check available liquidity in price ranges
//
// Run this example with:
//   cargo run --bin market_impact_simulation
//   (from the examples directory)

use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() {
    // Set up logging
    setup_logger();
    info!("Market Impact Simulation Example");

    // Create an order book with realistic market depth
    let book = create_orderbook_with_depth("BTC/USD");

    // Display current book state
    display_orderbook_state(&book);

    // Demonstrate market impact analysis
    demo_market_impact_analysis(&book);

    // Demonstrate order simulation
    demo_order_simulation(&book);

    // Demonstrate liquidity analysis
    demo_liquidity_analysis(&book);

    // Practical use case: Pre-trade risk assessment
    demo_pretrade_risk_assessment(&book);
}

fn create_orderbook_with_depth(symbol: &str) -> OrderBook {
    info!("\n=== Creating OrderBook with Market Depth ===");
    info!("Symbol: {}", symbol);

    let book = OrderBook::new(symbol);

    // Add buy orders (bids) with decreasing liquidity
    info!("\nAdding buy orders (bids):");
    let bid_orders = vec![
        (50000, 100), // Best bid
        (49950, 150),
        (49900, 200),
        (49850, 250),
        (49800, 300),
        (49750, 350),
    ];

    for (price, quantity) in bid_orders {
        let order_id = Id::new();
        let _ = book.add_limit_order(order_id, price, quantity, Side::Buy, TimeInForce::Gtc, None);
        info!("  Bid: {} @ {}", quantity, price);
    }

    // Add sell orders (asks) with decreasing liquidity
    info!("\nAdding sell orders (asks):");
    let ask_orders = vec![
        (50100, 120), // Best ask
        (50150, 180),
        (50200, 220),
        (50250, 280),
        (50300, 320),
        (50350, 380),
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

fn demo_market_impact_analysis(book: &OrderBook) {
    info!("\n=== Market Impact Analysis ===");
    info!("Analyzing the impact of different order sizes");

    let order_sizes = vec![100, 250, 500, 1000];

    // Analyze buy orders
    info!("\nBuy orders (executing against asks):");
    for size in &order_sizes {
        let impact = book.market_impact(*size, Side::Buy);

        info!("\n  Order size: {} units", size);
        info!("    Average execution price: {:.2}", impact.avg_price);
        info!("    Worst price: {}", impact.worst_price);
        info!(
            "    Slippage: {} ({:.2} bps)",
            impact.slippage, impact.slippage_bps
        );
        info!("    Levels consumed: {}", impact.levels_consumed);
        info!(
            "    Available liquidity: {}",
            impact.total_quantity_available
        );

        if impact.can_fill(*size) {
            info!("    ✓ Order can be fully filled");
        } else {
            info!(
                "    ✗ Insufficient liquidity (fill ratio: {:.2}%)",
                impact.fill_ratio(*size) * 100.0
            );
        }
    }

    // Analyze sell orders
    info!("\nSell orders (executing against bids):");
    for size in &order_sizes {
        let impact = book.market_impact(*size, Side::Sell);

        info!("\n  Order size: {} units", size);
        info!("    Average execution price: {:.2}", impact.avg_price);
        info!("    Worst price: {}", impact.worst_price);
        info!(
            "    Slippage: {} ({:.2} bps)",
            impact.slippage, impact.slippage_bps
        );
        info!("    Levels consumed: {}", impact.levels_consumed);

        if impact.can_fill(*size) {
            info!("    ✓ Order can be fully filled");
        } else {
            info!("    ✗ Insufficient liquidity");
        }
    }
}

fn demo_order_simulation(book: &OrderBook) {
    info!("\n=== Order Execution Simulation ===");
    info!("Step-by-step simulation of order execution");

    // Simulate a buy order
    let buy_quantity = 400;
    info!("\nSimulating buy order of {} units:", buy_quantity);

    let simulation = book.simulate_market_order(buy_quantity, Side::Buy);

    info!("  Execution details:");
    for (i, (price, qty)) in simulation.fills.iter().enumerate() {
        info!(
            "    Fill {}: {} units @ {} = {}",
            i + 1,
            qty,
            price,
            (*price as u128) * (*qty as u128)
        );
    }

    info!("\n  Summary:");
    info!("    Total filled: {} units", simulation.total_filled);
    info!("    Average price: {:.2}", simulation.avg_price);
    info!("    Remaining: {} units", simulation.remaining_quantity);
    info!("    Total cost: {}", simulation.total_cost());
    info!("    Levels used: {}", simulation.levels_count());

    if simulation.is_fully_filled() {
        info!("    ✓ Order fully filled");
    } else {
        info!("    ⚠ Partial fill only");
    }

    // Simulate a sell order
    let sell_quantity = 400;
    info!("\nSimulating sell order of {} units:", sell_quantity);

    let simulation = book.simulate_market_order(sell_quantity, Side::Sell);

    info!("  Execution details:");
    for (i, (price, qty)) in simulation.fills.iter().enumerate() {
        info!(
            "    Fill {}: {} units @ {} = {}",
            i + 1,
            qty,
            price,
            (*price as u128) * (*qty as u128)
        );
    }

    info!("\n  Summary:");
    info!("    Total filled: {} units", simulation.total_filled);
    info!("    Average price: {:.2}", simulation.avg_price);
    info!("    Total revenue: {}", simulation.total_cost());
}

fn demo_liquidity_analysis(book: &OrderBook) {
    info!("\n=== Liquidity Analysis ===");
    info!("Analyzing liquidity distribution across price ranges");

    // Analyze bid liquidity
    info!("\nBid side liquidity:");
    let bid_ranges = vec![
        (49950, 50000, "Near touch"),
        (49800, 50000, "Top 3 levels"),
        (49000, 50000, "Wide range"),
    ];

    for (min_price, max_price, description) in bid_ranges {
        let liquidity = book.liquidity_in_range(min_price, max_price, Side::Buy);
        info!(
            "  {} ({}-{}): {} units",
            description, min_price, max_price, liquidity
        );
    }

    // Analyze ask liquidity
    info!("\nAsk side liquidity:");
    let ask_ranges = vec![
        (50100, 50150, "Near touch"),
        (50100, 50300, "Top 3 levels"),
        (50100, 51000, "Wide range"),
    ];

    for (min_price, max_price, description) in ask_ranges {
        let liquidity = book.liquidity_in_range(min_price, max_price, Side::Sell);
        info!(
            "  {} ({}-{}): {} units",
            description, min_price, max_price, liquidity
        );
    }

    // Analyze specific price bands
    info!("\nPrice band analysis:");
    let bands = vec![
        (50000, 50100, "Spread zone"),
        (50100, 50200, "First 100 points above"),
        (50200, 50300, "Next 100 points"),
    ];

    for (min_price, max_price, description) in bands {
        let liquidity = book.liquidity_in_range(min_price, max_price, Side::Sell);
        info!("  {}: {} units", description, liquidity);
    }
}

fn demo_pretrade_risk_assessment(book: &OrderBook) {
    info!("\n=== Pre-Trade Risk Assessment ===");
    info!("Comprehensive analysis before order execution");

    let order_size = 600;
    let order_side = Side::Buy;

    info!("\nProposed order:");
    info!("  Size: {} units", order_size);
    info!("  Side: {:?}", order_side);

    // Step 1: Check market impact
    let impact = book.market_impact(order_size, order_side);

    info!("\n1. Market Impact Assessment:");
    info!("   Average execution price: {:.2}", impact.avg_price);
    info!("   Expected slippage: {:.2} bps", impact.slippage_bps);
    info!("   Price levels to consume: {}", impact.levels_consumed);

    // Step 2: Liquidity check
    let can_fill = impact.can_fill(order_size);
    let fill_ratio = impact.fill_ratio(order_size);

    info!("\n2. Liquidity Check:");
    if can_fill {
        info!("   ✓ Sufficient liquidity available");
        info!("   Can fill: 100%");
    } else {
        info!("   ⚠ Insufficient liquidity");
        info!("   Can fill: {:.1}%", fill_ratio * 100.0);
        info!(
            "   Shortfall: {} units",
            order_size - impact.total_quantity_available
        );
    }

    // Step 3: Cost estimation
    let simulation = book.simulate_market_order(order_size, order_side);
    let total_cost = simulation.total_cost();

    info!("\n3. Cost Estimation:");
    info!("   Total cost: {} units", total_cost);
    info!("   Average price: {:.2}", simulation.avg_price);

    if let Some(best_ask) = book.best_ask() {
        let best_cost = (best_ask as u128) * (order_size as u128);
        let additional_cost = total_cost.saturating_sub(best_cost);
        info!("   Cost at best price: {} units", best_cost);
        info!("   Additional cost (slippage): {} units", additional_cost);
    }

    // Step 4: Risk classification
    info!("\n4. Risk Classification:");
    if impact.slippage_bps < 10.0 {
        info!("   ✓ LOW RISK - Minimal slippage expected");
        info!("   → Safe to execute as market order");
    } else if impact.slippage_bps < 50.0 {
        info!("   • MEDIUM RISK - Moderate slippage");
        info!("   → Consider splitting order or using limit orders");
    } else {
        info!("   ✗ HIGH RISK - Significant slippage");
        info!("   → Strongly recommend order splitting");
        info!("   → Consider alternative execution strategies");
    }

    // Step 5: Execution recommendation
    info!("\n5. Execution Recommendation:");

    if !can_fill {
        info!("   ⚠ Order cannot be fully filled");
        info!(
            "   → Reduce order size to {} units",
            impact.total_quantity_available
        );
    } else if impact.slippage_bps > 50.0 {
        info!(
            "   → Split into smaller orders (suggested: {} units each)",
            order_size / 3
        );
        info!("   → Use TWAP or VWAP algorithm");
        info!("   → Time orders to minimize impact");
    } else if impact.slippage_bps > 10.0 {
        info!("   → Consider limit order at {:.2}", simulation.avg_price);
        info!("   → Or split into 2-3 smaller orders");
    } else {
        info!("   ✓ Execute as single market order");
        info!(
            "   Expected execution: {:.2} @ {} units",
            simulation.avg_price, simulation.total_filled
        );
    }
}
