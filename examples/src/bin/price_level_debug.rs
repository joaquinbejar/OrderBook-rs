// examples/src/bin/price_level_debug.rs

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
    info!("Price Level Distribution Debug Test");
    info!("===================================");

    // Only test the problematic case
    let price_levels = 100; // Test with 100 price levels, like the last successful case

    info!("\nTesting with {} price levels...", price_levels);

    // Create a fresh order book
    let order_book = Arc::new(OrderBook::new(SYMBOL));

    // Pre-populate with fewer orders to avoid memory issues
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

    info!("Starting threads...");

    // Spawn worker threads one by one with logs
    let mut handles = Vec::with_capacity(THREAD_COUNT);

    for thread_id in 0..THREAD_COUNT {
        let thread_book = Arc::clone(&order_book);
        let thread_barrier = Arc::clone(&barrier);
        let thread_running = Arc::clone(&running);

        info!("Spawning thread {}...", thread_id);

        let handle = thread::spawn(move || {
            info!("Thread {} waiting at barrier", thread_id);

            // Wait for synchronized start - with logs
            // BarrierWaitResult is not a Result type, so we don't need to match on Ok/Err
            let wait_result = thread_barrier.wait();

            if wait_result.is_leader() {
                info!("Thread {} is the barrier leader", thread_id);
            }
            info!("Thread {} passed barrier", thread_id);

            info!("Thread {} starting work", thread_id);

            let mut local_counter = 0;

            // Simplify the work loop for debugging
            while thread_running.load(Ordering::Relaxed) {
                // Only perform read operations to simplify
                let _ = thread_book.best_bid();
                let _ = thread_book.best_ask();

                local_counter += 1;

                // More frequent sleep
                if local_counter % 100 == 0 {
                    thread::sleep(Duration::from_micros(10));
                }
            }

            info!(
                "Thread {} completed with {} operations",
                thread_id, local_counter
            );
            local_counter
        });

        handles.push(handle);
        // Add a small delay between thread creation
        thread::sleep(Duration::from_millis(10));
    }

    // Wait a moment to ensure threads are ready
    thread::sleep(Duration::from_millis(100));

    // Start the test with logs
    info!("Main thread waiting at barrier");
    let start_time = Instant::now();

    info!("Main thread releasing barrier");
    barrier.wait();
    info!("Main thread passed barrier");

    // Run for a shorter duration
    info!("Test running for {} ms...", TEST_DURATION_MS);
    thread::sleep(Duration::from_millis(TEST_DURATION_MS));

    // Signal threads to stop
    info!("Stopping test...");
    running.store(false, Ordering::Relaxed);

    // Wait for all threads to finish
    for (i, handle) in handles.into_iter().enumerate() {
        info!("Waiting for thread {} to finish...", i);
        match handle.join() {
            Ok(count) => {
                info!("Thread {} completed with {} operations", i, count);
            }
            Err(_) => {
                info!("Thread {} panicked", i);
            }
        }
    }

    let elapsed = start_time.elapsed();
    info!("Test completed in {:?}", elapsed);
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
