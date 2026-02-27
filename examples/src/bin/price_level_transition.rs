// examples/src/bin/price_level_transition.rs

use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
use tracing::info;

const THREAD_COUNT: usize = 4; // Fewer threads for debugging
const TEST_DURATION_MS: u64 = 1000; // Shorter duration for faster testing
const SYMBOL: &str = "TEST/USD";

fn main() {
    setup_logger();
    info!("Price Level Transition Test");
    info!("===========================");

    // Try two different price level configurations
    let test_cases = [100, 5]; // Test with 100 levels, then 5 levels

    for &price_levels in &test_cases {
        info!("\nTesting with {} price levels...", price_levels);

        // Create a fresh order book for each test
        let order_book = Arc::new(OrderBook::new(SYMBOL));

        // Pre-populate with fewer orders
        let min_orders = 10; // 10 orders per level

        info!(
            "Setting up orders: {} per level x {} levels",
            min_orders, price_levels
        );
        setup_orders_for_test(&order_book, price_levels, min_orders);

        // Verify book state
        let snapshot = order_book.create_snapshot(price_levels as usize);
        info!(
            "Pre-populated with {} bid levels and {} ask levels",
            snapshot.bids.len(),
            snapshot.asks.len()
        );

        // Flag to signal when to stop the test
        let running = Arc::new(AtomicBool::new(true));

        // Barrier for synchronized start
        let barrier = Arc::new(Barrier::new(THREAD_COUNT + 1));

        info!("Starting threads for {} levels test...", price_levels);

        // Spawn worker threads
        let mut handles = Vec::with_capacity(THREAD_COUNT);

        for thread_id in 0..THREAD_COUNT {
            let thread_book = Arc::clone(&order_book);
            let thread_barrier = Arc::clone(&barrier);
            let thread_running = Arc::clone(&running);
            let max_level = price_levels;

            let handle = thread::spawn(move || {
                info!(
                    "Thread {} waiting at barrier for {} levels test",
                    thread_id, max_level
                );

                let wait_result = thread_barrier.wait();
                if wait_result.is_leader() {
                    info!(
                        "Thread {} is the barrier leader for {} levels test",
                        thread_id, max_level
                    );
                }

                info!(
                    "Thread {} starting work for {} levels test",
                    thread_id, max_level
                );

                let mut local_counter = 0u64;

                // Simplified work loop with a mix of operations
                while thread_running.load(Ordering::Relaxed) {
                    match local_counter % 10 {
                        0 => {
                            // Add a buy limit order
                            let id = Id::from_u64(thread_id as u64 * 1000000 + local_counter);
                            let level = local_counter % std::cmp::max(1, max_level as u64);
                            let price: u128 = 10000 - level as u128 * 10;
                            let _ = thread_book.add_limit_order(
                                id,
                                price,
                                10,
                                Side::Buy,
                                TimeInForce::Gtc,
                                None,
                            );
                        }
                        1 => {
                            // Add a sell limit order
                            let id = Id::from_u64(thread_id as u64 * 1000000 + local_counter);
                            let level = local_counter % std::cmp::max(1, max_level as u64);
                            let price: u128 = 10100 + level as u128 * 10;
                            let _ = thread_book.add_limit_order(
                                id,
                                price,
                                10,
                                Side::Sell,
                                TimeInForce::Gtc,
                                None,
                            );
                        }
                        2 => {
                            // Submit a small market buy order
                            let id = Id::from_u64(thread_id as u64 * 1000000 + local_counter);
                            let _ = thread_book.submit_market_order(id, 1, Side::Buy);
                        }
                        3 => {
                            // Submit a small market sell order
                            let id = Id::from_u64(thread_id as u64 * 1000000 + local_counter);
                            let _ = thread_book.submit_market_order(id, 1, Side::Sell);
                        }
                        // The rest are read operations
                        _ => {
                            let _ = thread_book.best_bid();
                            let _ = thread_book.best_ask();
                            let _ = thread_book.spread();
                            let _ = thread_book.mid_price();
                        }
                    }

                    local_counter += 1;

                    if local_counter % 100 == 0 {
                        thread::sleep(Duration::from_micros(10));
                    }
                }

                info!(
                    "Thread {} completed with {} operations for {} levels test",
                    thread_id, local_counter, max_level
                );
                local_counter
            });

            handles.push(handle);
            thread::sleep(Duration::from_millis(10));
        }

        // Wait for threads to be ready
        thread::sleep(Duration::from_millis(100));

        // Start the test
        info!(
            "Main thread waiting at barrier for {} levels test",
            price_levels
        );
        let start_time = Instant::now();

        info!(
            "Main thread releasing barrier for {} levels test",
            price_levels
        );
        barrier.wait();
        info!(
            "Main thread passed barrier for {} levels test",
            price_levels
        );

        // Run for the specified duration
        info!(
            "Test running for {} ms with {} levels...",
            TEST_DURATION_MS, price_levels
        );
        thread::sleep(Duration::from_millis(TEST_DURATION_MS));

        // Signal threads to stop
        info!("Stopping test with {} levels...", price_levels);
        running.store(false, Ordering::Relaxed);

        // Wait for all threads to finish
        let mut total_ops = 0;
        for (i, handle) in handles.into_iter().enumerate() {
            info!(
                "Waiting for thread {} to finish {} levels test...",
                i, price_levels
            );
            match handle.join() {
                Ok(count) => {
                    info!(
                        "Thread {} completed with {} operations for {} levels test",
                        i, count, price_levels
                    );
                    total_ops += count;
                }
                Err(_) => {
                    info!("Thread {} panicked in {} levels test", i, price_levels);
                }
            }
        }

        let elapsed = start_time.elapsed();
        let ops_per_second = total_ops as f64 / elapsed.as_secs_f64();

        info!(
            "Test with {} levels completed in {:?}",
            price_levels, elapsed
        );
        info!("Total operations: {}", total_ops);
        info!("Operations per second: {:.2}", ops_per_second);
    }

    info!("All tests completed successfully");
}

/// Sets up orders for the price level test
fn setup_orders_for_test(order_book: &OrderBook, price_levels: i32, orders_per_level: i32) {
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
}
