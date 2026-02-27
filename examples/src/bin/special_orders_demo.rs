//! Demonstration of special order types: PeggedOrder and TrailingStop
//!
//! This example shows how to use and re-price special order types that
//! automatically adjust their prices based on market conditions.
//!
//! # Order Types Demonstrated:
//!
//! ## PeggedOrder
//! Orders that track a reference price (best bid, best ask, mid price, or last trade)
//! with an optional offset. When the reference price changes, the order price
//! can be automatically adjusted.
//!
//! ## TrailingStop
//! Stop orders that follow the market price with a fixed trail amount.
//! - **Sell trailing stop**: Trails below the market high, adjusts upward when market rises
//! - **Buy trailing stop**: Trails above the market low, adjusts downward when market falls
//!
//! # Usage:
//! ```bash
//! cargo run --manifest-path examples/Cargo.toml --bin special_orders_demo
//! ```

use orderbook_rs::orderbook::repricing::RepricingOperations;
use orderbook_rs::prelude::*;
use pricelevel::{Hash32, OrderType, PegReferenceType, Price, Quantity, TimestampMs, setup_logger};
use tracing::info;

type OrderBook = orderbook_rs::OrderBook<()>;

fn main() {
    setup_logger();

    info!("=== Special Orders Demo ===");
    info!("Demonstrating PeggedOrder and TrailingStop order types\n");

    demo_pegged_orders();
    demo_trailing_stop_orders();
    demo_combined_repricing();

    info!("\n=== Demo Complete ===");
}

fn demo_pegged_orders() {
    info!("\n--- Pegged Orders Demo ---");
    info!("Pegged orders track a reference price with an optional offset.\n");

    let book = OrderBook::new("BTC/USD");

    // First, establish market liquidity
    info!("Step 1: Establishing market liquidity...");

    // Add buy orders (bids)
    for i in 0u64..5 {
        let price: u128 = 50000 - (i as u128 * 100); // 50000, 49900, 49800, ...
        let id = Id::from_u64(i + 1);
        let _ = book.add_limit_order(id, price, 10, Side::Buy, TimeInForce::Gtc, None);
    }

    // Add sell orders (asks)
    for i in 0u64..5 {
        let price: u128 = 50100 + (i as u128 * 100); // 50100, 50200, 50300, ...
        let id = Id::from_u64(i + 100);
        let _ = book.add_limit_order(id, price, 10, Side::Sell, TimeInForce::Gtc, None);
    }

    info!(
        "  Best Bid: {} | Best Ask: {}",
        book.best_bid().unwrap_or(0),
        book.best_ask().unwrap_or(0)
    );
    info!("  Mid Price: {:.2}", book.mid_price().unwrap_or(0.0));

    // Add a pegged order that tracks best bid + 50
    info!("\nStep 2: Adding pegged order (tracks Best Bid + 50)...");

    let pegged_id = Id::from_u64(1000);
    let pegged_order = OrderType::PeggedOrder {
        id: pegged_id,
        price: Price::new(49000), // Initial price (will be re-priced)
        quantity: Quantity::new(5),
        side: Side::Buy,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(current_time_millis()),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: 50, // +50 from reference
        reference_price_type: PegReferenceType::BestBid,
        extra_fields: (),
    };

    book.add_order(pegged_order).unwrap();
    info!("  Pegged order added with initial price: 49000");
    info!("  Tracked pegged orders: {}", book.pegged_order_count());

    // Re-price the pegged order
    info!("\nStep 3: Re-pricing pegged order...");
    let repriced = book.reprice_pegged_orders().unwrap();
    info!("  Orders re-priced: {}", repriced);

    if let Some(order) = book.get_order(pegged_id) {
        info!(
            "  New price: {} (Best Bid {} + offset 50)",
            order.price(),
            book.best_bid().unwrap_or(0)
        );
    }

    // Demonstrate different reference types
    info!("\n--- Pegged Order Reference Types ---");
    info!("  BestBid: Tracks the highest buy price");
    info!("  BestAsk: Tracks the lowest sell price");
    info!("  MidPrice: Tracks the midpoint between best bid and ask");
    info!("  LastTrade: Tracks the last executed trade price");
}

fn demo_trailing_stop_orders() {
    info!("\n--- Trailing Stop Orders Demo ---");
    info!("Trailing stops follow the market with a fixed trail amount.\n");

    let book = OrderBook::new("ETH/USD");

    // Establish market
    info!("Step 1: Establishing market at 3000...");

    for i in 0u64..5 {
        let bid_price: u128 = 3000 - (i as u128 * 10);
        let ask_price: u128 = 3010 + (i as u128 * 10);
        let _ = book.add_limit_order(
            Id::from_u64(i + 1),
            bid_price,
            100,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );
        let _ = book.add_limit_order(
            Id::from_u64(i + 100),
            ask_price,
            100,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
    }

    info!(
        "  Best Bid: {} | Best Ask: {}",
        book.best_bid().unwrap_or(0),
        book.best_ask().unwrap_or(0)
    );

    // Add a sell trailing stop at a price ABOVE best bid (won't match)
    // Sell orders only match with buy orders if sell_price <= buy_price
    // So we need stop_price > best_bid to avoid matching
    info!("\nStep 2: Adding SELL trailing stop (trail amount: 50)...");
    info!("  This stop trails BELOW the market high.");
    info!("  When market rises, the stop price rises with it.");

    let trailing_sell_id = Id::from_u64(2000);
    let trailing_sell = OrderType::TrailingStop {
        id: trailing_sell_id,
        price: Price::new(3050), // Stop price ABOVE best bid (3000), won't match
        quantity: Quantity::new(10),
        side: Side::Sell,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(current_time_millis()),
        time_in_force: TimeInForce::Gtc,
        trail_amount: Quantity::new(50),
        last_reference_price: Price::new(3100), // Market high was at 3100
        extra_fields: (),
    };

    book.add_order(trailing_sell).unwrap();
    info!("  Trailing stop added: stop at 3050 (market high 3100 - trail 50)");
    info!("  Tracked trailing stops: {}", book.trailing_stop_count());

    // Simulate market rising by adding higher bids
    info!("\nStep 3: Simulating market rise to 3200...");

    // Add new higher bids to simulate market rise
    for i in 0u64..5 {
        let price: u128 = 3200 - (i as u128 * 10);
        let _ = book.add_limit_order(
            Id::from_u64(i + 200),
            price,
            50,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );
    }

    let new_best_bid = book.best_bid().unwrap_or(0);
    info!("  New Best Bid: {} (rose from 3100)", new_best_bid);

    // Re-price trailing stops
    info!("\nStep 4: Re-pricing trailing stops...");
    info!("  Market rose from 3100 to {}", new_best_bid);
    info!(
        "  Stop should adjust: {} - 50 = {}",
        new_best_bid,
        new_best_bid - 50
    );
    let repriced = book.reprice_trailing_stops().unwrap();
    info!("  Stops re-priced: {}", repriced);

    if let Some(order) = book.get_order(trailing_sell_id) {
        info!(
            "  New stop price: {} (was 3050, now {} - 50 = {})",
            order.price(),
            new_best_bid,
            new_best_bid.saturating_sub(50)
        );
    }

    // Demonstrate trigger check
    info!("\n--- Trailing Stop Trigger Check ---");
    if let Some(order) = book.get_order(trailing_sell_id) {
        let current_bid = book.best_bid().unwrap_or(0);
        let would_trigger = book.should_trigger_trailing_stop(&order, current_bid);
        info!(
            "  Current market: {} | Stop price: {} | Would trigger: {}",
            current_bid,
            order.price(),
            would_trigger
        );

        // Check at a lower price (below stop)
        let lower_price = order.price().as_u128() - 100;
        let would_trigger_lower = book.should_trigger_trailing_stop(&order, lower_price);
        info!(
            "  If market falls to {}: Would trigger: {}",
            lower_price, would_trigger_lower
        );
    }
}

fn demo_combined_repricing() {
    info!("\n--- Combined Re-pricing Demo ---");
    info!("Re-pricing all special orders at once.\n");

    let book = OrderBook::new("SOL/USD");

    // Establish market
    for i in 0u64..3 {
        let bid_price: u128 = 100 - (i as u128 * 5);
        let ask_price: u128 = 105 + (i as u128 * 5);
        let _ = book.add_limit_order(
            Id::from_u64(i + 1),
            bid_price,
            100,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );
        let _ = book.add_limit_order(
            Id::from_u64(i + 100),
            ask_price,
            100,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
    }

    info!(
        "Market: Best Bid {} | Best Ask {}",
        book.best_bid().unwrap_or(0),
        book.best_ask().unwrap_or(0)
    );

    // Add multiple special orders
    let pegged1 = OrderType::PeggedOrder {
        id: Id::from_u64(1000),
        price: Price::new(90),
        quantity: Quantity::new(10),
        side: Side::Buy,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(current_time_millis()),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: 2,
        reference_price_type: PegReferenceType::BestBid,
        extra_fields: (),
    };

    let pegged2 = OrderType::PeggedOrder {
        id: Id::from_u64(1001),
        price: Price::new(110),
        quantity: Quantity::new(10),
        side: Side::Sell,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(current_time_millis()),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: -2,
        reference_price_type: PegReferenceType::BestAsk,
        extra_fields: (),
    };

    let trailing = OrderType::TrailingStop {
        id: Id::from_u64(2000),
        price: Price::new(110), // Above best bid (100), won't match
        quantity: Quantity::new(10),
        side: Side::Sell,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(current_time_millis()),
        time_in_force: TimeInForce::Gtc,
        trail_amount: Quantity::new(5),
        last_reference_price: Price::new(99), // Market was at 99
        extra_fields: (),
    };

    book.add_order(pegged1).unwrap();
    book.add_order(pegged2).unwrap();
    book.add_order(trailing).unwrap();

    info!("\nAdded special orders:");
    info!("  Pegged orders: {}", book.pegged_order_count());
    info!("  Trailing stops: {}", book.trailing_stop_count());

    // Re-price all at once
    info!("\nRe-pricing all special orders...");
    let result = book.reprice_special_orders().unwrap();

    info!("Results:");
    info!(
        "  Pegged orders re-priced: {}",
        result.pegged_orders_repriced
    );
    info!(
        "  Trailing stops re-priced: {}",
        result.trailing_stops_repriced
    );

    // Show final prices
    info!("\nFinal order prices:");
    for id in book.pegged_order_ids() {
        if let Some(order) = book.get_order(id) {
            info!("  Pegged {}: price = {}", id, order.price());
        }
    }
    for id in book.trailing_stop_ids() {
        if let Some(order) = book.get_order(id) {
            info!("  Trailing {}: stop price = {}", id, order.price());
        }
    }

    // Best practices
    info!("\n--- Best Practices ---");
    info!("1. Call reprice_special_orders() after significant market changes");
    info!("2. Use price_level_changed_listener to trigger re-pricing automatically");
    info!("3. Check should_trigger_trailing_stop() before executing stops");
    info!("4. Consider using a timer to periodically re-price orders");
}

fn current_time_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
