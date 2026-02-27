//! Example demonstrating real-time trade monitoring with TradeListener
//!
//! This example shows how to:
//! 1. Create an order book with a TradeListener
//! 2. Fill the order book with limit orders
//! 3. Execute limit orders that cross the market and generate trades
//! 4. Display trade information in real-time as matches occur

use orderbook_rs::{Id, OrderBook, Side, TimeInForce, TradeListener, TradeResult};
use pricelevel::setup_logger;
use std::sync::Arc;
use tracing::info;

fn main() {
    // Set up logging
    setup_logger();
    info!("=== Trade Listener Demo ===\n");

    // Create a trade listener that displays trades in real-time
    let trade_listener: TradeListener = Arc::new(|trade_result: &TradeResult| {
        display_trade_event(trade_result);
    });

    // Create order book with trade listener
    let book = OrderBook::with_trade_listener("ETH/USD", trade_listener);

    // Step 1: Fill the order book with initial liquidity
    info!("Step 1: Adding initial liquidity to the order book");
    info!("================================================\n");
    fill_orderbook_with_liquidity(&book);

    // Display initial order book state
    display_orderbook_summary(&book);

    // Step 2: Execute crossing limit orders that will generate trades
    info!("\nStep 2: Executing limit orders that cross the market");
    info!("================================================");
    info!("Note: TradeListener will display trades in real-time\n");

    execute_crossing_limit_orders(&book);

    // Step 3: Execute more aggressive orders
    info!("\nStep 3: Executing additional crossing orders");
    info!("================================================\n");

    execute_additional_orders(&book);

    // Display final order book state
    info!("\n");
    display_orderbook_summary(&book);

    info!("\n=== Demo Complete ===");
}

/// Fill the order book with initial bid and ask orders
fn fill_orderbook_with_liquidity(book: &OrderBook) {
    // Add bid orders (buy side)
    info!("Adding BID orders (buy side):");
    let bid_orders = vec![
        (3000, 50), // price, quantity
        (2980, 75),
        (2960, 100),
        (2940, 125),
        (2920, 150),
    ];

    for (i, (price, quantity)) in bid_orders.iter().enumerate() {
        let order_id = Id::from_u64(1000 + i as u64);
        match book.add_limit_order(
            order_id,
            *price,
            *quantity,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        ) {
            Ok(_) => info!("  ✓ BID: {} units @ ${}", quantity, price),
            Err(e) => info!("  ✗ Failed to add BID: {}", e),
        }
    }

    info!("\nAdding ASK orders (sell side):");
    let ask_orders = vec![
        (3020, 50), // price, quantity
        (3040, 75),
        (3060, 100),
        (3080, 125),
        (3100, 150),
    ];

    for (i, (price, quantity)) in ask_orders.iter().enumerate() {
        let order_id = Id::from_u64(2000 + i as u64);
        match book.add_limit_order(
            order_id,
            *price,
            *quantity,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        ) {
            Ok(_) => info!("  ✓ ASK: {} units @ ${}", quantity, price),
            Err(e) => info!("  ✗ Failed to add ASK: {}", e),
        }
    }
}

/// Execute limit orders that cross the market and trigger trades
fn execute_crossing_limit_orders(book: &OrderBook) {
    // Aggressive buy order that crosses the spread
    info!("🔵 Adding aggressive BUY limit order @ $3050 for 100 units");
    info!("   (This will match against ASK orders at $3020 and $3040)\n");

    let buy_order_id = Id::from_u64(5000);
    match book.add_limit_order(
        buy_order_id,
        3050, // Price above best ask - will match
        100,
        Side::Buy,
        TimeInForce::Gtc,
        None,
    ) {
        Ok(order) => {
            info!(
                "\n✓ Order executed. Remaining quantity in book: {}",
                order.visible_quantity()
            );
        }
        Err(e) => info!("\n✗ Order failed: {}", e),
    }

    // Wait a moment for clarity
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Aggressive sell order that crosses the spread
    info!("\n🔴 Adding aggressive SELL limit order @ $2970 for 120 units");
    info!("   (This will match against BID orders at $3000 and $2980)\n");

    let sell_order_id = Id::from_u64(5001);
    match book.add_limit_order(
        sell_order_id,
        2970, // Price below best bid - will match
        120,
        Side::Sell,
        TimeInForce::Gtc,
        None,
    ) {
        Ok(order) => {
            info!(
                "\n✓ Order executed. Remaining quantity in book: {}",
                order.visible_quantity()
            );
        }
        Err(e) => info!("\n✗ Order failed: {}", e),
    }
}

/// Execute additional crossing orders
fn execute_additional_orders(book: &OrderBook) {
    // Large buy order that sweeps multiple levels
    info!("🔵 Adding large BUY limit order @ $3100 for 200 units");
    info!("   (This will sweep through multiple ASK levels)\n");

    let buy_order_id = Id::from_u64(5002);
    match book.add_limit_order(
        buy_order_id,
        3100, // High price - will match multiple levels
        200,
        Side::Buy,
        TimeInForce::Gtc,
        None,
    ) {
        Ok(order) => {
            info!(
                "\n✓ Order executed. Remaining quantity in book: {}",
                order.visible_quantity()
            );
        }
        Err(e) => info!("\n✗ Order failed: {}", e),
    }

    // Wait a moment for clarity
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Another aggressive sell order
    info!("\n🔴 Adding SELL limit order @ $2900 for 180 units");
    info!("   (This will match against remaining BID orders)\n");

    let sell_order_id = Id::from_u64(5003);
    match book.add_limit_order(
        sell_order_id,
        2900, // Low price - will match
        180,
        Side::Sell,
        TimeInForce::Gtc,
        None,
    ) {
        Ok(order) => {
            info!(
                "\n✓ Order executed. Remaining quantity in book: {}",
                order.visible_quantity()
            );
        }
        Err(e) => info!("\n✗ Order failed: {}", e),
    }
}

/// Display trade event information (called by TradeListener)
fn display_trade_event(trade_result: &TradeResult) {
    let match_result = &trade_result.match_result;

    info!("");
    info!("┌─────────────────────────────────────────────────────────────┐");
    info!(
        "│ 💰 TRADE EVENT - Symbol: {}                         │",
        trade_result.symbol
    );
    info!("├─────────────────────────────────────────────────────────────┤");
    info!("│ Order ID:           {}        │", match_result.order_id());
    info!(
        "│ Executed Quantity:  {} units                              │",
        match_result.executed_quantity().unwrap_or(0)
    );
    info!(
        "│ Remaining Quantity: {} units                               │",
        match_result.remaining_quantity()
    );
    info!(
        "│ Complete:           {}                                   │",
        match_result.is_complete()
    );
    info!(
        "│ Transactions:       {}                                     │",
        match_result.trades().as_vec().len()
    );
    info!("├─────────────────────────────────────────────────────────────┤");

    if !match_result.trades().as_vec().is_empty() {
        info!("│ Transaction Details:                                        │");
        for (idx, tx) in match_result.trades().as_vec().iter().enumerate() {
            info!(
                "│   [{}] Price: ${:<6} | Quantity: {:<4} units              │",
                idx + 1,
                tx.price(),
                tx.quantity()
            );
            info!(
                "│       Maker: {} │",
                format_order_id(&tx.maker_order_id().to_string())
            );
            info!(
                "│       Taker: {} │",
                format_order_id(&tx.taker_order_id().to_string())
            );
            if idx < match_result.trades().as_vec().len() - 1 {
                info!("│       ─────────────────────────────────────────────────     │");
            }
        }
    }

    info!("└─────────────────────────────────────────────────────────────┘");
}

/// Format order ID for display (show first and last parts)
fn format_order_id(order_id: &str) -> String {
    if order_id.len() > 36 {
        format!("{}...", &order_id[..36])
    } else {
        order_id.to_string()
    }
}

/// Display order book summary
fn display_orderbook_summary(book: &OrderBook) {
    info!("\n📊 Order Book Summary - {}", book.symbol());
    info!("═══════════════════════════════════════");

    if let Some(best_bid) = book.best_bid() {
        info!("  Best BID:    ${}", best_bid);
    } else {
        info!("  Best BID:    None");
    }

    if let Some(best_ask) = book.best_ask() {
        info!("  Best ASK:    ${}", best_ask);
    } else {
        info!("  Best ASK:    None");
    }

    if let Some(spread) = book.spread() {
        info!("  Spread:      ${}", spread);
    } else {
        info!("  Spread:      N/A");
    }

    if let Some(mid_price) = book.mid_price() {
        info!("  Mid Price:   ${:.2}", mid_price);
    }

    if let Some(last_trade) = book.last_trade_price() {
        info!("  Last Trade:  ${}", last_trade);
    }

    // Display volume by price
    let (bid_volumes, ask_volumes) = book.get_volume_by_price();

    if !bid_volumes.is_empty() {
        info!("\n  📈 BID Levels ({} levels):", bid_volumes.len());
        let mut bid_prices: Vec<_> = bid_volumes.keys().collect();
        bid_prices.sort_by(|a, b| b.cmp(a)); // Descending
        for price in bid_prices.iter().take(5) {
            if let Some(volume) = bid_volumes.get(price) {
                info!("     ${:>5} │ {:>4} units", price, volume);
            }
        }
    } else {
        info!("\n  📈 BID Levels: Empty");
    }

    if !ask_volumes.is_empty() {
        info!("\n  📉 ASK Levels ({} levels):", ask_volumes.len());
        let mut ask_prices: Vec<_> = ask_volumes.keys().collect();
        ask_prices.sort(); // Ascending
        for price in ask_prices.iter().take(5) {
            if let Some(volume) = ask_volumes.get(price) {
                info!("     ${:>5} │ {:>4} units", price, volume);
            }
        }
    } else {
        info!("\n  📉 ASK Levels: Empty");
    }

    info!("═══════════════════════════════════════");
}
