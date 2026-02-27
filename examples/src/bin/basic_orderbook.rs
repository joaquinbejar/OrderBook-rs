// examples/src/bin/basic_orderbook.rs

use orderbook_rs::{OrderBook, current_time_millis};
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() {
    // Set up logging
    setup_logger();
    info!("Basic OrderBook Example");

    // Create a new order book for a symbol
    let book = create_orderbook("BTC/USD");

    // Add various types of orders to the book
    demo_adding_orders(&book);

    // Display current book state
    display_orderbook_state(&book);

    // Demonstrate order lookup and retrieval
    demo_order_lookup(&book);

    // Demonstrate market order submission
    demo_market_orders(&book);

    // Demonstrate limit order matching
    demo_limit_order_matching(&book);

    // Demonstrate order cancellation
    demo_cancel_orders(&book);

    // Display final book state
    info!("\nFinal OrderBook State:");
    display_orderbook_state(&book);
}

fn create_orderbook(symbol: &str) -> OrderBook {
    info!("Creating OrderBook for symbol: {}", symbol);
    let book = OrderBook::new(symbol);

    // Set market close timestamp for DAY orders (e.g., 8 hours from now)
    let current_time = current_time_millis();
    let market_close = current_time + (8 * 60 * 60 * 1000); // 8 hours in milliseconds
    book.set_market_close_timestamp(market_close);

    info!("Created OrderBook with market close at: {}", market_close);
    book
}

fn demo_adding_orders(book: &OrderBook) {
    info!("\nAdding orders to the OrderBook...");

    // Add some buy limit orders at different price levels
    for i in 0u64..5 {
        let price: u128 = 9900 + (i as u128 * 20); // 9900, 9920, 9940, 9960, 9980
        let quantity: u64 = 10 + (i * 5); // 10, 15, 20, 25, 30
        let id = new_order_id();

        let result = book.add_limit_order(id, price, quantity, Side::Buy, TimeInForce::Gtc, None);

        match result {
            Ok(order) => info!(
                "Added BUY limit order: id={}, price={}, qty={}",
                order.id(),
                order.price(),
                order.visible_quantity()
            ),
            Err(e) => info!("Failed to add BUY limit order: {}", e),
        }
    }

    // Add some sell limit orders at different price levels
    for i in 0u64..5 {
        let price: u128 = 10000 + (i as u128 * 20); // 10000, 10020, 10040, 10060, 10080
        let quantity: u64 = 10 + (i * 5); // 10, 15, 20, 25, 30
        let id = new_order_id();

        let result = book.add_limit_order(id, price, quantity, Side::Sell, TimeInForce::Gtc, None);

        match result {
            Ok(order) => info!(
                "Added SELL limit order: id={}, price={}, qty={}",
                order.id(),
                order.price(),
                order.visible_quantity()
            ),
            Err(e) => info!("Failed to add SELL limit order: {}", e),
        }
    }

    // Add an iceberg order
    let id = new_order_id();
    let result = book.add_iceberg_order(id, 9990, 5, 45, Side::Buy, TimeInForce::Gtc, None);

    match result {
        Ok(order) => info!(
            "Added iceberg order: id={}, price={}, visible={}, hidden={}",
            order.id(),
            order.price(),
            order.visible_quantity(),
            order.hidden_quantity()
        ),
        Err(e) => info!("Failed to add iceberg order: {}", e),
    }

    // Add a post-only order
    let id = new_order_id();
    let result = book.add_post_only_order(id, 10100, 20, Side::Sell, TimeInForce::Gtc, None);

    match result {
        Ok(order) => info!(
            "Added post-only order: id={}, price={}, qty={}",
            order.id(),
            order.price(),
            order.visible_quantity()
        ),
        Err(e) => info!("Failed to add post-only order: {}", e),
    }

    // Try to add a post-only order that would cross the market (should fail)
    let id = new_order_id();
    let result = book.add_post_only_order(id, 9980, 10, Side::Sell, TimeInForce::Gtc, None);

    match result {
        Ok(_) => info!("Added post-only order (unexpected)"),
        Err(e) => info!("Failed to add crossing post-only order as expected: {}", e),
    }

    // Add a Fill-or-Kill order
    let id = new_order_id();
    let result = book.add_limit_order(id, 9970, 5, Side::Sell, TimeInForce::Fok, None);

    match result {
        Ok(order) => info!(
            "Added FOK order: id={}, price={}, qty={}",
            order.id(),
            order.price(),
            order.visible_quantity()
        ),
        Err(e) => info!("Failed to add FOK order: {}", e),
    }

    // Add an Immediate-or-Cancel order
    let id = new_order_id();
    let result = book.add_limit_order(id, 9975, 8, Side::Sell, TimeInForce::Ioc, None);

    match result {
        Ok(order) => info!(
            "Added IOC order: id={}, price={}, qty={}",
            order.id(),
            order.price(),
            order.visible_quantity()
        ),
        Err(e) => info!("Failed to add IOC order: {}", e),
    }
}

fn demo_order_lookup(book: &crate::OrderBook) {
    info!("\nDemonstrating order lookup...");

    // Get best bid and ask
    let best_bid = book.best_bid();
    let best_ask = book.best_ask();

    info!("Best bid: {:?}", best_bid);
    info!("Best ask: {:?}", best_ask);

    // Calculate spread and mid price
    let spread = book.spread();
    let mid_price = book.mid_price();

    info!("Spread: {:?}", spread);
    info!("Mid price: {:?}", mid_price);

    // Get all orders at a specific price level
    if let Some(bid_price) = best_bid {
        let orders = book.get_orders_at_price(bid_price, Side::Buy);
        info!("Orders at best bid ({}): {}", bid_price, orders.len());

        for (i, order) in orders.iter().enumerate().take(3) {
            info!(
                "  Order {}: id={}, qty={}",
                i,
                order.id(),
                order.visible_quantity()
            );
        }
    }

    if let Some(ask_price) = best_ask {
        let orders = book.get_orders_at_price(ask_price, Side::Sell);
        info!("Orders at best ask ({}): {}", ask_price, orders.len());

        for (i, order) in orders.iter().enumerate().take(3) {
            info!(
                "  Order {}: id={}, qty={}",
                i,
                order.id(),
                order.visible_quantity()
            );
        }
    }

    // Create a snapshot of the order book
    let snapshot = book.create_snapshot(5); // Get top 5 price levels

    info!("OrderBook snapshot:");
    info!("  Symbol: {}", snapshot.symbol);
    info!("  Timestamp: {}", snapshot.timestamp);
    info!("  Bids: {} levels", snapshot.bids.len());
    info!("  Asks: {} levels", snapshot.asks.len());

    // Get volume by price
    let (bid_volumes, ask_volumes) = book.get_volume_by_price();

    info!("Volume by price:");
    info!("  Bid price levels: {}", bid_volumes.len());
    info!("  Ask price levels: {}", ask_volumes.len());
}

fn demo_market_orders(book: &crate::OrderBook) {
    info!("\nDemonstrating market orders...");

    // Submit market buy order
    let id = new_order_id();
    let result = book.submit_market_order(id, 25, Side::Buy);

    match result {
        Ok(match_result) => {
            info!(
                "Market BUY result: executed={}, remaining={}, complete={}, transactions={}",
                match_result.executed_quantity().unwrap_or(0),
                match_result.remaining_quantity(),
                match_result.is_complete(),
                match_result.trades().as_vec().len()
            );

            // Display transaction details
            for (i, tx) in match_result.trades().as_vec().iter().enumerate() {
                info!(
                    "  Transaction {}: price={}, qty={}, taker={}, maker={}",
                    i,
                    tx.price(),
                    tx.quantity(),
                    tx.taker_order_id(),
                    tx.maker_order_id()
                );
            }
        }
        Err(e) => info!("Market BUY failed: {}", e),
    }

    // Submit market sell order
    let id = new_order_id();
    let result = book.submit_market_order(id, 40, Side::Sell);

    match result {
        Ok(match_result) => {
            info!(
                "Market SELL result: executed={}, remaining={}, complete={}, transactions={}",
                match_result.executed_quantity().unwrap_or(0),
                match_result.remaining_quantity(),
                match_result.is_complete(),
                match_result.trades().as_vec().len()
            );
        }
        Err(e) => info!("Market SELL failed: {}", e),
    }

    // Try a market order that would have insufficient liquidity
    let id = new_order_id();
    let result = book.submit_market_order(id, 1000, Side::Buy);

    match result {
        Ok(match_result) => {
            info!(
                "Large market BUY result: executed={}, remaining={}, complete={}",
                match_result.executed_quantity().unwrap_or(0),
                match_result.remaining_quantity(),
                match_result.is_complete()
            );
        }
        Err(e) => info!("Large market BUY failed as expected: {}", e),
    }
}

fn demo_limit_order_matching(book: &crate::OrderBook) {
    info!("\nDemonstrating limit order matching...");

    // Add a limit order that would cross the market (automatic execution)
    let id = new_order_id();
    let result = book.add_limit_order(id, 10040, 15, Side::Buy, TimeInForce::Gtc, None);

    match result {
        Ok(order) => info!(
            "Added crossing limit order: id={}, price={}, qty={}",
            order.id(),
            order.price(),
            order.visible_quantity()
        ),
        Err(e) => info!("Failed to add crossing limit order: {}", e),
    }

    // Add an IOC order that should partially execute
    let id = new_order_id();
    let result = book.add_limit_order(id, 10060, 50, Side::Buy, TimeInForce::Ioc, None);

    match result {
        Ok(order) => info!(
            "Added IOC order: id={}, price={}, qty={}",
            order.id(),
            order.price(),
            order.visible_quantity()
        ),
        Err(e) => info!("Failed to add IOC order: {}", e),
    }

    // Add a FOK order that should fully execute
    let id = new_order_id();
    let result = book.add_limit_order(id, 10080, 10, Side::Buy, TimeInForce::Fok, None);

    match result {
        Ok(order) => info!(
            "Added FOK order: id={}, price={}, qty={}",
            order.id(),
            order.price(),
            order.visible_quantity()
        ),
        Err(e) => info!("Failed to add FOK order: {}", e),
    }

    // Add a FOK order that shouldn't execute (not enough liquidity)
    let id = new_order_id();
    let result = book.add_limit_order(id, 10080, 100, Side::Buy, TimeInForce::Fok, None);

    match result {
        Ok(_) => info!("Added FOK order (unexpected)"),
        Err(e) => info!("Failed to add FOK order as expected: {}", e),
    }
}

fn demo_cancel_orders(book: &crate::OrderBook) {
    info!("\nDemonstrating order cancellation...");

    // Add an order to cancel later
    let id = new_order_id();
    let result = book.add_limit_order(id, 9850, 30, Side::Buy, TimeInForce::Gtc, None);

    let order_id = match result {
        Ok(order) => {
            info!(
                "Added order to cancel: id={}, price={}, qty={}",
                order.id(),
                order.price(),
                order.visible_quantity()
            );
            order.id()
        }
        Err(e) => {
            info!("Failed to add order: {}", e);
            return;
        }
    };

    // Cancel the order
    let result = book.cancel_order(order_id);

    match result {
        Ok(Some(order)) => info!(
            "Successfully cancelled order: id={}, price={}, qty={}",
            order.id(),
            order.price(),
            order.visible_quantity()
        ),
        Ok(None) => info!("Order not found for cancellation"),
        Err(e) => info!("Failed to cancel order: {}", e),
    }

    // Try to cancel a non-existent order
    let result = book.cancel_order(new_order_id());

    match result {
        Ok(Some(_)) => info!("Cancelled non-existent order (unexpected)"),
        Ok(None) => info!("Non-existent order not found for cancellation, as expected"),
        Err(e) => info!("Error cancelling non-existent order: {}", e),
    }
}

fn display_orderbook_state(book: &crate::OrderBook) {
    info!("\nOrderBook State for {}:", book.symbol());

    // Display best prices
    match (book.best_bid(), book.best_ask()) {
        (Some(bid), Some(ask)) => {
            info!("Best bid: {}", bid);
            info!("Best ask: {}", ask);
            info!("Spread: {}", ask - bid);
            info!("Mid price: {:.2}", (bid as f64 + ask as f64) / 2.0);
        }
        (Some(bid), None) => {
            info!("Best bid: {}", bid);
            info!("No asks present");
        }
        (None, Some(ask)) => {
            info!("No bids present");
            info!("Best ask: {}", ask);
        }
        (None, None) => {
            info!("No orders in the book");
        }
    }

    // Get all orders
    let all_orders = book.get_all_orders();
    info!("Total orders: {}", all_orders.len());

    // Display last trade price if available
    if let Some(last_trade) = book.last_trade_price() {
        info!("Last trade price: {}", last_trade);
    } else {
        info!("No trades executed yet");
    }

    // Create a detailed snapshot
    let snapshot = book.create_snapshot(3); // Top 3 levels

    info!("Bids:");
    for (i, level) in snapshot.bids.iter().enumerate() {
        info!(
            "  Level {}: price={}, visible={}, hidden={}, orders={}",
            i,
            level.price(),
            level.visible_quantity(),
            level.hidden_quantity(),
            level.order_count()
        );
    }

    info!("Asks:");
    for (i, level) in snapshot.asks.iter().enumerate() {
        info!(
            "  Level {}: price={}, visible={}, hidden={}, orders={}",
            i,
            level.price(),
            level.visible_quantity(),
            level.hidden_quantity(),
            level.order_count()
        );
    }
}

fn new_order_id() -> Id {
    Id::new_uuid()
}
