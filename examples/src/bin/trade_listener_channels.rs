//! Example demonstrating TradeListener usage with channels for multi-book management
//!
//! This example shows how to:
//! 1. Use TradeListener with channels for async communication
//! 2. Manage multiple order books with symbol-aware trade routing
//! 3. Use BookManager to handle trades from multiple symbols
//! 4. Demonstrate real-world patterns for trading systems

use orderbook_rs::prelude::{BookManager, BookManagerStd, Id, Side, TimeInForce};
use std::thread;
use std::time::Duration;
use tracing::{info, warn};

/// Example helper to add liquidity to an order book
fn add_liquidity(book: &orderbook_rs::OrderBook<()>, symbol: &str) {
    info!("Adding liquidity to {}", symbol);

    // Add some ask orders (sell side)
    for i in 1u64..=5 {
        let order_id = Id::from_u64(1000 + i);
        let price: u128 = 50000 + (i as u128 * 10); // Prices: 50010, 50020, 50030, etc.
        let quantity = 100;

        if let Err(e) = book.add_limit_order(
            order_id,
            price,
            quantity,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        ) {
            warn!("Failed to add ask order {}: {}", order_id, e);
        }
    }

    // Add some bid orders (buy side)
    for i in 1u64..=5 {
        let order_id = Id::from_u64(2000 + i);
        let price: u128 = 49990 - (i as u128 * 10); // Prices: 49980, 49970, 49960, etc.
        let quantity = 100;

        if let Err(e) =
            book.add_limit_order(order_id, price, quantity, Side::Buy, TimeInForce::Gtc, None)
        {
            warn!("Failed to add bid order {}: {}", order_id, e);
        }
    }
}

/// Execute some trades to demonstrate the trade listener
fn execute_trades(book: &orderbook_rs::OrderBook<()>, symbol: &str) {
    info!("Executing trades on {}", symbol);

    // Execute a buy market order that will match against asks
    let buy_order_id = Id::from_u64(3001);
    if let Err(e) =
        book.add_limit_order(buy_order_id, 50020, 150, Side::Buy, TimeInForce::Gtc, None)
    {
        warn!("Failed to execute buy order: {}", e);
    }

    thread::sleep(Duration::from_millis(100)); // Allow processing

    // Execute a sell market order that will match against bids
    let sell_order_id = Id::from_u64(3002);
    if let Err(e) = book.add_limit_order(
        sell_order_id,
        49980,
        120,
        Side::Sell,
        TimeInForce::Gtc,
        None,
    ) {
        warn!("Failed to execute sell order: {}", e);
    }

    thread::sleep(Duration::from_millis(100)); // Allow processing
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("Starting TradeListener channels example");

    // Create a BookManagerStd with unit type (no extra data)
    let mut manager = BookManagerStd::<()>::new();

    // Add multiple order books
    let symbols = vec!["BTC/USD", "ETH/USD", "SOL/USD"];
    for symbol in &symbols {
        manager.add_book(symbol);
    }

    // Start the trade processor
    let _processor_handle = manager.start_trade_processor();

    // Add liquidity to all books
    for symbol in &symbols {
        if let Some(book) = manager.get_book(symbol) {
            add_liquidity(book, symbol);
        }
    }

    info!("Liquidity added to all books");
    thread::sleep(Duration::from_millis(500));

    // Execute trades on different books
    for symbol in &symbols {
        if let Some(book) = manager.get_book(symbol) {
            execute_trades(book, symbol);
            thread::sleep(Duration::from_millis(200));
        }
    }

    // Show book states
    for symbol in &symbols {
        if let Some(book) = manager.get_book(symbol) {
            info!(
                "{} - Best Bid: {:?}, Best Ask: {:?}, Spread: {:?}",
                symbol,
                book.best_bid(),
                book.best_ask(),
                book.spread()
            );
        }
    }

    // Wait a bit more for all events to be processed
    thread::sleep(Duration::from_secs(1));

    info!("Example completed successfully");

    // Note: In a real application, you'd want to gracefully shutdown
    // the trade processor thread, but for this example we'll just let it finish

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orderbook_rs::prelude::{OrderBook, TradeListener, TradeResult};
    use std::sync::{Arc, mpsc};

    #[test]
    fn test_book_manager_with_channels() {
        let mut manager = BookManagerStd::<()>::new();

        // Add a book
        manager.add_book("TEST/USD");

        // Verify book was added
        assert!(manager.get_book("TEST/USD").is_some());
        assert!(manager.get_book("NONEXISTENT").is_none());
    }

    #[test]
    fn test_trade_listener_with_channel() {
        let (sender, receiver) = mpsc::channel::<TradeResult>();

        // Create a trade listener that sends to our test channel
        let trade_listener: TradeListener = Arc::new(move |trade_result: &TradeResult| {
            sender.send(trade_result.clone()).unwrap();
        });

        let book = OrderBook::<()>::with_trade_listener("TEST/USD", trade_listener);

        // Add liquidity
        let ask_id = Id::from_u64(1);
        book.add_limit_order(ask_id, 100, 50, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // Execute a trade
        let buy_id = Id::from_u64(2);
        book.add_limit_order(buy_id, 100, 30, Side::Buy, TimeInForce::Gtc, None)
            .unwrap();

        // Verify we received the trade event
        let trade_result = receiver.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(trade_result.symbol, "TEST/USD");
        assert_eq!(
            trade_result.match_result.executed_quantity().unwrap_or(0),
            30
        );
    }
}
