//! Example demonstrating market order execution and trade display
//!
//! This example shows how to:
//! 1. Fill an order book with bid and ask limit orders
//! 2. Execute market orders to generate trades
//! 3. Display the resulting trades with detailed information
//! 4. Use TradeListener to capture trades in real-time

use orderbook_rs::prelude::{
    Id, OrderBook, Side, TimeInForce, TradeInfo, TradeListener, TradeResult,
};
use pricelevel::{Quantity, setup_logger};
use std::sync::{Arc, Mutex};
use tracing::info;

fn main() {
    // Set up logging
    let _ = setup_logger();
    info!("=== Market Trades Demo ===\n");

    // Create a container to store all trades
    let trades: Arc<Mutex<Vec<TradeInfo>>> = Arc::new(Mutex::new(Vec::new()));
    let trades_clone = Arc::clone(&trades);

    // Create a trade listener that captures all trades
    let trade_listener: TradeListener = Arc::new(move |trade_result: &TradeResult| {
        let trade_info = create_trade_info_from_result(trade_result);
        trades_clone.lock().unwrap().push(trade_info);
    });

    // Create order book with trade listener
    let book = OrderBook::with_trade_listener("BTC/USD", trade_listener);

    // Step 1: Fill the order book with bid orders
    info!("Step 1: Adding BID orders (buy side)");
    info!("----------------------------------------");
    add_bid_orders(&book);
    info!("");

    // Step 2: Fill the order book with ask orders
    info!("Step 2: Adding ASK orders (sell side)");
    info!("----------------------------------------");
    add_ask_orders(&book);
    info!("");

    // Display the order book state
    display_orderbook_state(&book);

    // Step 3: Execute market orders to generate trades
    info!("\nStep 3: Executing MARKET orders");
    info!("----------------------------------------");
    info!("Note: TradeListener will capture trades automatically\n");
    execute_market_orders(&book);
    info!("");

    // Display the order book state after trades
    display_orderbook_state(&book);

    // Step 4: Display all trades
    info!("\nStep 4: Trade Summary");
    info!("========================================");
    display_trades(&trades);

    info!("\n=== Demo Complete ===");
}

/// Add bid orders (buy side) to the order book
fn add_bid_orders(book: &OrderBook) {
    let bid_levels = vec![
        (49900, 100), // price, quantity
        (49850, 150),
        (49800, 200),
        (49750, 250),
        (49700, 300),
    ];

    for (i, (price, quantity)) in bid_levels.iter().enumerate() {
        let order_id = Id::from_u64(1000 + i as u64);

        match book.add_limit_order(
            order_id,
            *price,
            *quantity,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        ) {
            Ok(_) => info!(
                "  ✓ Added BID order #{}: {} units @ ${} (ID: {})",
                i + 1,
                quantity,
                price,
                order_id
            ),
            Err(e) => info!("  ✗ Failed to add BID order: {}", e),
        }
    }
}

/// Add ask orders (sell side) to the order book
fn add_ask_orders(book: &OrderBook) {
    let ask_levels = vec![
        (50100, 100), // price, quantity
        (50150, 150),
        (50200, 200),
        (50250, 250),
        (50300, 300),
    ];

    for (i, (price, quantity)) in ask_levels.iter().enumerate() {
        let order_id = Id::from_u64(2000 + i as u64);

        match book.add_limit_order(
            order_id,
            *price,
            *quantity,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        ) {
            Ok(_) => info!(
                "  ✓ Added ASK order #{}: {} units @ ${} (ID: {})",
                i + 1,
                quantity,
                price,
                order_id
            ),
            Err(e) => info!("  ✗ Failed to add ASK order: {}", e),
        }
    }
}

/// Execute market orders to generate trades
fn execute_market_orders(book: &OrderBook) {
    // Market buy order - will match against asks
    info!("  Executing MARKET BUY order for 250 units...");
    let buy_order_id = Id::from_u64(3000);

    match book.submit_market_order(buy_order_id, 250, Side::Buy) {
        Ok(match_result) => {
            info!(
                "  ✓ Market BUY executed: {} units filled, {} transactions",
                match_result.executed_quantity().unwrap_or(Quantity::new(0)),
                match_result.trades().len()
            );
        }
        Err(e) => info!("  ✗ Market BUY failed: {}", e),
    }

    // Market sell order - will match against bids
    info!("\n  Executing MARKET SELL order for 300 units...");
    let sell_order_id = Id::from_u64(3001);

    match book.submit_market_order(sell_order_id, 300, Side::Sell) {
        Ok(match_result) => {
            info!(
                "  ✓ Market SELL executed: {} units filled, {} transactions",
                match_result.executed_quantity().unwrap_or(Quantity::new(0)),
                match_result.trades().len()
            );
        }
        Err(e) => info!("  ✗ Market SELL failed: {}", e),
    }

    // Another market buy order
    info!("\n  Executing MARKET BUY order for 180 units...");
    let buy_order_id_2 = Id::from_u64(3002);

    match book.submit_market_order(buy_order_id_2, 180, Side::Buy) {
        Ok(match_result) => {
            info!(
                "  ✓ Market BUY executed: {} units filled, {} transactions",
                match_result.executed_quantity().unwrap_or(Quantity::new(0)),
                match_result.trades().len()
            );
        }
        Err(e) => info!("  ✗ Market BUY failed: {}", e),
    }
}

/// Helper function to create TradeInfo from TradeResult.
///
/// Delegates to the engine-provided `TradeInfo::from_trade_result`, which
/// populates the per-transaction maker/taker fees from the supplied fee
/// schedule. This demo runs without a fee schedule, so it passes `None`
/// (per-transaction fees are `0`); pass `Some(&schedule)` to populate them.
fn create_trade_info_from_result(trade_result: &TradeResult) -> TradeInfo {
    TradeInfo::from_trade_result(trade_result, None)
}

/// Display the current state of the order book
fn display_orderbook_state(book: &OrderBook) {
    info!("\n📊 Order Book State for {}", book.symbol());
    info!("========================================");

    if let Some(best_bid) = book.best_bid() {
        info!("  Best BID: ${}", best_bid);
    } else {
        info!("  Best BID: None");
    }

    if let Some(best_ask) = book.best_ask() {
        info!("  Best ASK: ${}", best_ask);
    } else {
        info!("  Best ASK: None");
    }

    if let Some(spread) = book.spread() {
        info!("  Spread:   ${}", spread);
    } else {
        info!("  Spread:   N/A");
    }

    if let Some(mid_price) = book.mid_price() {
        info!("  Mid Price: ${:.2}", mid_price);
    } else {
        info!("  Mid Price: N/A");
    }

    if let Some(last_trade) = book.last_trade_price() {
        info!("  Last Trade: ${}", last_trade);
    } else {
        info!("  Last Trade: None");
    }

    // Display volume by price
    let (bid_volumes, ask_volumes) = book.get_volume_by_price();

    if !bid_volumes.is_empty() {
        info!("\n  BID Levels:");
        let mut bid_prices: Vec<_> = bid_volumes.keys().collect();
        bid_prices.sort_by(|a, b| b.cmp(a)); // Descending
        for price in bid_prices.iter().take(5) {
            if let Some(volume) = bid_volumes.get(price) {
                info!("    ${:>6} | {:>4} units", price, volume);
            }
        }
    }

    if !ask_volumes.is_empty() {
        info!("\n  ASK Levels:");
        let mut ask_prices: Vec<_> = ask_volumes.keys().collect();
        ask_prices.sort(); // Ascending
        for price in ask_prices.iter().take(5) {
            if let Some(volume) = ask_volumes.get(price) {
                info!("    ${:>6} | {:>4} units", price, volume);
            }
        }
    }
}

/// Display all captured trades with detailed information
fn display_trades(trades: &Arc<Mutex<Vec<TradeInfo>>>) {
    let trades_vec = trades.lock().unwrap();

    if trades_vec.is_empty() {
        info!("  No trades executed.");
        return;
    }

    info!("  Total trades executed: {}\n", trades_vec.len());

    for (idx, trade) in trades_vec.iter().enumerate() {
        info!("Trade #{}", idx + 1);
        info!("  Symbol:             {}", trade.symbol);
        info!("  Order ID:           {}", trade.order_id);
        info!("  Executed Quantity:  {} units", trade.executed_quantity);
        info!("  Remaining Quantity: {} units", trade.remaining_quantity);
        info!("  Complete:           {}", trade.is_complete);
        info!("  Transactions:       {}", trade.transaction_count);

        if !trade.transactions.is_empty() {
            info!("\n  Transaction Details:");
            for (tx_idx, tx) in trade.transactions.iter().enumerate() {
                info!("    Transaction #{}:", tx_idx + 1);
                info!("      Price:          ${}", tx.price);
                info!("      Quantity:       {} units", tx.quantity);
                info!("      Transaction ID: {}", tx.transaction_id);
                info!("      Maker Order:    {}", tx.maker_order_id);
                info!("      Taker Order:    {}", tx.taker_order_id);
            }
        }

        info!(""); // Empty line between trades
    }

    // Calculate and display summary statistics
    let total_volume: u64 = trades_vec.iter().map(|t| t.executed_quantity).sum();
    let total_transactions: usize = trades_vec.iter().map(|t| t.transaction_count).sum();

    info!("Summary Statistics:");
    info!("  Total Volume:       {} units", total_volume);
    info!("  Total Transactions: {}", total_transactions);
    info!(
        "  Average per Trade:  {:.2} units",
        total_volume as f64 / trades_vec.len() as f64
    );
}
