//! Example demonstrating the use of the prelude module
//!
//! This example shows how to use the prelude to import all commonly used types
//! with a single import statement.

// Import everything from the prelude
use orderbook_rs::prelude::*;

fn main() {
    // Set up logging
    pricelevel::setup_logger();

    println!("=== Prelude Demo ===\n");
    println!("This example demonstrates using the prelude module for convenient imports.\n");

    // Create an order book using types from the prelude
    let book: DefaultOrderBook = OrderBook::new("ETH/USD");

    println!("✓ Created OrderBook for {}", book.symbol());

    // Add some orders using types imported via prelude
    let order_id_1 = Id::from_u64(1);
    let result = book.add_limit_order(order_id_1, 3000, 100, Side::Buy, TimeInForce::Gtc, None);

    match result {
        Ok(_) => println!("✓ Added BUY order: 100 units @ $3000"),
        Err(e) => println!("✗ Failed to add order: {}", e),
    }

    let order_id_2 = Id::from_u64(2);
    let result = book.add_limit_order(order_id_2, 3100, 100, Side::Sell, TimeInForce::Gtc, None);

    match result {
        Ok(_) => println!("✓ Added SELL order: 100 units @ $3100"),
        Err(e) => println!("✗ Failed to add order: {}", e),
    }

    // Display order book state
    println!("\nOrder Book State:");
    println!("  Best BID: ${:?}", book.best_bid());
    println!("  Best ASK: ${:?}", book.best_ask());
    println!("  Spread:   ${:?}", book.spread());
    println!("  Mid Price: ${:?}", book.mid_price());

    // Get current time using utility from prelude
    let timestamp = current_time_millis();
    println!("\nCurrent timestamp: {}", timestamp);

    // Demonstrate using DefaultOrderBook type alias
    let default_book: DefaultOrderBook = OrderBook::new("BTC/USD");
    println!("\n✓ Created DefaultOrderBook for {}", default_book.symbol());

    println!("\n=== All types imported via prelude! ===");
    println!("\nThe following types were available without explicit imports:");
    println!("  - OrderBook");
    println!("  - Id");
    println!("  - Side");
    println!("  - TimeInForce");
    println!("  - DefaultOrderBook");
    println!("  - current_time_millis");
    println!("\nThis makes code cleaner and easier to write!");
}
