// examples/src/bin/enriched_snapshots.rs
//
// This example demonstrates enriched snapshots with pre-calculated metrics.
// Enriched snapshots provide better performance than creating a snapshot and
// calculating metrics separately, as they compute everything in a single pass.
//
// Functions demonstrated:
// - `enriched_snapshot()`: Snapshot with all metrics pre-calculated
// - `enriched_snapshot_with_metrics()`: Custom metric selection for optimization
// - `MetricFlags`: Bitflags for controlling which metrics to calculate
//
// Run this example with:
//   cargo run --bin enriched_snapshots
//   (from the examples directory)

use orderbook_rs::{MetricFlags, OrderBook};
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() {
    // Set up logging
    setup_logger();
    info!("Enriched Snapshots Example");

    // Create order book with realistic depth
    let book = create_orderbook_with_depth("BTC/USD");

    // Display current state
    display_book_state(&book);

    // Demonstrate full snapshot with all metrics
    demo_full_enriched_snapshot(&book);

    // Demonstrate custom metric selection
    demo_custom_metrics(&book);

    // Performance comparison
    demo_performance_benefits(&book);

    // Practical use cases
    demo_practical_use_cases(&book);

    // Market data distribution
    demo_market_data_distribution(&book);
}

fn create_orderbook_with_depth(symbol: &str) -> OrderBook {
    info!("\n=== Creating OrderBook ===");
    info!("Symbol: {}", symbol);

    let book = OrderBook::new(symbol);

    // Add buy orders with realistic distribution
    info!("\nAdding buy orders (bids):");
    let bid_orders = vec![
        (50000, 5), // Best bid
        (49990, 8),
        (49980, 12),
        (49970, 15),
        (49960, 20),
        (49950, 25),
        (49940, 18),
        (49930, 22),
        (49920, 16),
        (49910, 14),
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

    // Add sell orders with realistic distribution
    info!("\nAdding sell orders (asks):");
    let ask_orders = vec![
        (50010, 6), // Best ask
        (50020, 9),
        (50030, 13),
        (50040, 17),
        (50050, 21),
        (50060, 24),
        (50070, 19),
        (50080, 23),
        (50090, 17),
        (50100, 15),
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
    }
}

fn demo_full_enriched_snapshot(book: &OrderBook) {
    info!("\n=== Full Enriched Snapshot ===");
    info!("Creating snapshot with ALL metrics pre-calculated");

    // Create enriched snapshot with all metrics
    let snapshot = book.enriched_snapshot(10);

    info!("\n📊 Snapshot Metrics:");
    info!("  Symbol: {}", snapshot.symbol);
    info!("  Timestamp: {}", snapshot.timestamp);
    info!("  Bid levels: {}", snapshot.bids.len());
    info!("  Ask levels: {}", snapshot.asks.len());

    if let Some(mid) = snapshot.mid_price {
        info!("\n💰 Mid Price: {:.2}", mid);
    }

    if let Some(spread) = snapshot.spread_bps {
        info!("📏 Spread: {:.2} bps", spread);
    }

    info!("\n📈 Depth:");
    info!("  Bid depth total: {} units", snapshot.bid_depth_total);
    info!("  Ask depth total: {} units", snapshot.ask_depth_total);

    if let Some(vwap_bid) = snapshot.vwap_bid {
        info!("\n📊 VWAP:");
        info!("  Bid VWAP (top 10): {:.2}", vwap_bid);
    }
    if let Some(vwap_ask) = snapshot.vwap_ask {
        info!("  Ask VWAP (top 10): {:.2}", vwap_ask);
    }

    info!("\n⚖️  Imbalance: {:.3}", snapshot.order_book_imbalance);
    if snapshot.order_book_imbalance > 0.2 {
        info!("  → Buy pressure detected");
    } else if snapshot.order_book_imbalance < -0.2 {
        info!("  → Sell pressure detected");
    } else {
        info!("  → Balanced market");
    }

    info!("\n✨ Key Benefit: All metrics calculated in SINGLE PASS!");
}

fn demo_custom_metrics(book: &OrderBook) {
    info!("\n=== Custom Metric Selection ===");
    info!("Optimize performance by selecting only needed metrics");

    // Example 1: Only price metrics
    info!("\n1️⃣  Price Metrics Only (MID_PRICE + SPREAD):");
    let snapshot =
        book.enriched_snapshot_with_metrics(10, MetricFlags::MID_PRICE | MetricFlags::SPREAD);

    if let Some(mid) = snapshot.mid_price {
        info!("  Mid price: {:.2}", mid);
    }
    if let Some(spread) = snapshot.spread_bps {
        info!("  Spread: {:.2} bps", spread);
    }
    info!("  ✓ Faster than calculating all metrics");

    // Example 2: Only depth
    info!("\n2️⃣  Depth Metrics Only:");
    let snapshot = book.enriched_snapshot_with_metrics(10, MetricFlags::DEPTH);

    info!("  Bid depth: {}", snapshot.bid_depth_total);
    info!("  Ask depth: {}", snapshot.ask_depth_total);
    info!("  ✓ Perfect for liquidity monitoring");

    // Example 3: Only VWAP
    info!("\n3️⃣  VWAP Only:");
    let snapshot = book.enriched_snapshot_with_metrics(10, MetricFlags::VWAP);

    if let Some(vwap_bid) = snapshot.vwap_bid {
        info!("  Bid VWAP: {:.2}", vwap_bid);
    }
    if let Some(vwap_ask) = snapshot.vwap_ask {
        info!("  Ask VWAP: {:.2}", vwap_ask);
    }
    info!("  ✓ Ideal for execution benchmarks");

    // Example 4: Combination
    info!("\n4️⃣  Custom Combination (DEPTH + IMBALANCE + VWAP):");
    let snapshot = book.enriched_snapshot_with_metrics(
        10,
        MetricFlags::DEPTH | MetricFlags::IMBALANCE | MetricFlags::VWAP,
    );

    info!("  Bid depth: {}", snapshot.bid_depth_total);
    info!("  Ask depth: {}", snapshot.ask_depth_total);
    info!("  Imbalance: {:.3}", snapshot.order_book_imbalance);
    if let Some(vwap_bid) = snapshot.vwap_bid {
        info!("  Bid VWAP: {:.2}", vwap_bid);
    }
    info!("  ✓ Custom metrics for specific strategy");
}

fn demo_performance_benefits(book: &OrderBook) {
    info!("\n=== Performance Benefits ===");

    info!("\n📊 Traditional Approach (multiple passes):");
    info!("  1. Create snapshot");
    info!("  2. Calculate mid price (pass 1)");
    info!("  3. Calculate spread (pass 2)");
    info!("  4. Calculate depth (pass 3)");
    info!("  5. Calculate VWAP (pass 4)");
    info!("  6. Calculate imbalance (pass 5)");
    info!("  ❌ Result: 5+ separate passes through data");

    info!("\n⚡ Enriched Snapshot Approach:");
    let snapshot = book.enriched_snapshot(10);
    info!("  ✓ Create snapshot with ALL metrics");
    info!("  ✅ Result: SINGLE pass through data!");

    info!("\n🎯 Performance Advantages:");
    info!("  • Single pass vs multiple passes");
    info!("  • Better CPU cache locality");
    info!("  • Reduced memory allocations");
    info!("  • Lower latency for HFT");
    info!("  • Consistent timestamp for all metrics");

    // Demonstrate serialization
    info!("\n💾 Serialization Support:");
    match serde_json::to_string(&snapshot) {
        Ok(json) => {
            info!("  ✓ Snapshot serialized: {} bytes", json.len());
            info!("  → Can distribute enriched snapshots over network");
            info!("  → Receivers don't need to recalculate metrics");
        }
        Err(e) => {
            info!("  ✗ Serialization failed: {}", e);
        }
    }
}

fn demo_practical_use_cases(book: &OrderBook) {
    info!("\n=== Practical Use Cases ===");

    // Use case 1: HFT trading decision
    info!("\n1️⃣  HFT Trading Decision:");
    let snapshot = book.enriched_snapshot_with_metrics(
        5,
        MetricFlags::MID_PRICE | MetricFlags::SPREAD | MetricFlags::IMBALANCE,
    );

    if let (Some(mid), Some(spread)) = (snapshot.mid_price, snapshot.spread_bps) {
        info!("  Mid price: {:.2}", mid);
        info!("  Spread: {:.2} bps", spread);
        info!("  Imbalance: {:.3}", snapshot.order_book_imbalance);

        // Decision logic
        if spread < 20.0 && snapshot.order_book_imbalance.abs() < 0.1 {
            info!("  ✅ DECISION: Tight spread + balanced → Market make");
            let bid_price = (mid - 5.0) as u64;
            let ask_price = (mid + 5.0) as u64;
            info!("  → Place bid @ {}, ask @ {}", bid_price, ask_price);
        } else if snapshot.order_book_imbalance > 0.3 {
            info!("  ✅ DECISION: Strong buy pressure → Join bids");
        } else if snapshot.order_book_imbalance < -0.3 {
            info!("  ✅ DECISION: Strong sell pressure → Join asks");
        } else {
            info!("  ⚠️  DECISION: Wide spread or imbalanced → Wait");
        }
    }

    // Use case 2: Market data distribution
    info!("\n2️⃣  Market Data Distribution:");
    let _snapshot = book.enriched_snapshot(10);

    info!("  Distributing snapshot to subscribers...");
    info!("  ✓ Subscribers receive pre-calculated metrics");
    info!("  ✓ No need for clients to recalculate");
    info!("  ✓ Consistent metrics across all clients");
    info!("  ✓ Lower bandwidth (single snapshot vs multiple queries)");

    // Use case 3: Risk monitoring
    info!("\n3️⃣  Risk Monitoring:");
    let risk_snapshot =
        book.enriched_snapshot_with_metrics(10, MetricFlags::DEPTH | MetricFlags::VWAP);

    info!("  Monitoring liquidity risk...");
    if risk_snapshot.bid_depth_total < 100 || risk_snapshot.ask_depth_total < 100 {
        info!("  ⚠️  WARNING: Low liquidity detected!");
        info!("  → Bid depth: {}", risk_snapshot.bid_depth_total);
        info!("  → Ask depth: {}", risk_snapshot.ask_depth_total);
        info!("  → Recommendation: Reduce position sizes");
    } else {
        info!("  ✓ Adequate liquidity");
        info!("  → Bid depth: {}", risk_snapshot.bid_depth_total);
        info!("  → Ask depth: {}", risk_snapshot.ask_depth_total);
    }

    // Use case 4: Execution quality
    info!("\n4️⃣  Execution Quality Analysis:");
    let snapshot = book.enriched_snapshot(10);
    if let (Some(vwap_bid), Some(vwap_ask), Some(mid)) =
        (snapshot.vwap_bid, snapshot.vwap_ask, snapshot.mid_price)
    {
        info!("  Analyzing execution quality...");
        info!("  Mid price: {:.2}", mid);
        info!("  VWAP bid: {:.2}", vwap_bid);
        info!("  VWAP ask: {:.2}", vwap_ask);

        let buy_slippage = ((vwap_ask - mid) / mid) * 10000.0;
        let sell_slippage = ((mid - vwap_bid) / mid) * 10000.0;

        info!("  Expected slippage:");
        info!("    Buy:  {:.2} bps", buy_slippage);
        info!("    Sell: {:.2} bps", sell_slippage);
    }
}

fn demo_market_data_distribution(book: &OrderBook) {
    info!("\n=== Market Data Distribution Workflow ===");

    // Scenario: Market data provider distributing enriched snapshots
    info!("\n📡 Market Data Provider Workflow:");

    info!("\nStep 1: Create enriched snapshot");
    let snapshot = book.enriched_snapshot(10);
    info!("  ✓ Snapshot created with all metrics");

    info!("\nStep 2: Serialize for distribution");
    let json = serde_json::to_string_pretty(&snapshot).unwrap();
    info!("  ✓ Serialized to JSON ({} bytes)", json.len());

    info!("\nStep 3: Distribute to clients");
    info!("  → WebSocket broadcast");
    info!("  → REST API endpoint");
    info!("  → Message queue");

    info!("\n📥 Client-Side Benefits:");
    info!("  ✅ Receive ready-to-use metrics");
    info!("  ✅ No computation overhead");
    info!("  ✅ Consistent timestamp");
    info!("  ✅ Guaranteed consistency across clients");
    info!("  ✅ Lower latency (no recalculation)");

    // Example: Multiple clients with different needs
    info!("\n👥 Different Client Types:");

    info!("\n  Client A (Market Maker):");
    info!("    Needs: mid_price, spread_bps");
    info!("    ✓ Already in snapshot");
    info!("    → Immediate decision making");

    info!("\n  Client B (Risk Manager):");
    info!("    Needs: bid_depth_total, ask_depth_total");
    info!("    ✓ Already in snapshot");
    info!("    → Instant risk assessment");

    info!("\n  Client C (Quant Trader):");
    info!("    Needs: vwap, imbalance");
    info!("    ✓ Already in snapshot");
    info!("    → Strategy execution without delay");

    info!("\n✨ Summary:");
    info!("  • ONE snapshot serves ALL clients");
    info!("  • Each client extracts needed metrics");
    info!("  • No redundant calculations");
    info!("  • Optimal resource usage");
}
