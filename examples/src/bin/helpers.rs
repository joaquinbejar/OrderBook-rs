use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce};
use tracing::info;

// Helper function to set up orders for read/write test
pub fn setup_orders_for_read_write_test(order_book: &OrderBook) {
    // Add 500 orders across different price levels
    info!("Setting up orders for read/write test...");

    // Buy orders
    for i in 0u64..250 {
        let price: u128 = 9900 + (i as u128 % 20) * 5; // 20 price levels: 9900-9995
        let id = Id::from_u64(i);
        let quantity: u64 = 10 + (i % 10);

        let _ = order_book.add_limit_order(id, price, quantity, Side::Buy, TimeInForce::Gtc, None);
    }

    // Sell orders
    for i in 0u64..250 {
        let price: u128 = 10000 + (i as u128 % 20) * 5; // 20 price levels: 10000-10095
        let id = Id::from_u64(i + 250);
        let quantity: u64 = 10 + (i % 10);

        let _ = order_book.add_limit_order(id, price, quantity, Side::Sell, TimeInForce::Gtc, None);
    }

    info!("Read/write test setup complete: 500 orders added across 40 price levels");
}

// Helper function to set up orders for hot spot test
pub fn setup_orders_for_hot_spot_test(order_book: &crate::OrderBook) {
    // Add 500 orders, the first 20 will be the "hot spot"
    info!("Setting up orders for hot spot contention test...");

    // Hot spot orders (ID 0-19)
    for i in 0..20 {
        let is_buy = i % 2 == 0;
        let side = if is_buy { Side::Buy } else { Side::Sell };
        let price: u128 = if is_buy { 9950 } else { 10050 };
        let id = Id::from_u64(i as u64);

        let _ = order_book.add_limit_order(id, price, 10, side, TimeInForce::Gtc, None);
    }

    // Remaining orders (ID 20-499)
    for i in 20..500 {
        let is_buy = i % 2 == 0;
        let side = if is_buy { Side::Buy } else { Side::Sell };
        let price_base: u128 = if is_buy { 9900 } else { 10000 };
        let price_offset: u128 = (i % 100) * 1;
        let price: u128 = if is_buy {
            price_base - price_offset
        } else {
            price_base + price_offset
        };
        let id = Id::from_u64(i as u64);

        let _ = order_book.add_limit_order(id, price, 10, side, TimeInForce::Gtc, None);
    }

    info!("Hot spot test setup complete: 20 hot spot orders + 480 regular orders");
}

// Helper function to set up orders for price level test
pub fn setup_orders_for_price_level_test(
    order_book: &crate::OrderBook,
    price_levels: i32,
    min_orders_per_level: i32,
) {
    // Now we receive a parameter for the minimum number of orders per level
    info!(
        "Setting up orders for price level distribution test ({} levels)...",
        price_levels
    );

    // For the special case of 1 price level, create many orders
    let orders_per_level = if price_levels == 1 {
        min_orders_per_level * 2 // Double for the special case
    } else {
        min_orders_per_level
    };

    // Asegurar suficiente liquidez
    info!(
        "Adding {} orders per price level ({} levels)",
        orders_per_level, price_levels
    );

    let mut order_id = 0;

    // Buy orders
    for level in 0..price_levels {
        let price: u128 = 10000 - (level as u128 * 10);

        for _ in 0..orders_per_level {
            let id = Id::from_u64(order_id);
            order_id += 1;

            let _ = order_book.add_limit_order(id, price, 10, Side::Buy, TimeInForce::Gtc, None);
        }
    }

    // Sell orders
    for level in 0..price_levels {
        let price: u128 = 10100 + (level as u128 * 10);

        for _ in 0..orders_per_level {
            let id = Id::from_u64(order_id);
            order_id += 1;

            let _ = order_book.add_limit_order(id, price, 10, Side::Sell, TimeInForce::Gtc, None);
        }
    }

    let total_orders = orders_per_level * price_levels * 2; // 2 lados (compra/venta)
    info!(
        "Price level test setup complete: {} orders added across {} price levels",
        total_orders,
        price_levels * 2
    );
}

#[allow(dead_code)]
fn main() {}
