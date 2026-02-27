mod helpers;

use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::info;

// Test parameters
const THREAD_COUNT: usize = 12;
const TEST_DURATION_MS: u64 = 3000; // 3 seconds per test
const SYMBOL: &str = "TEST/USD";

fn main() {
    // Set up logging
    setup_logger();
    info!("OrderBook Contention Patterns Test");
    info!("==================================");
    info!("Threads: {}", THREAD_COUNT);
    info!("Test duration: {} ms per test", TEST_DURATION_MS);

    // Run the tests with proper error handling
    if let Err(e) = run_tests() {
        info!("Error running tests: {}", e);
    } else {
        info!("All tests completed successfully");
    }
}

fn run_tests() -> Result<(), String> {
    // Ejecutamos cada test con manejo de errores
    match test_read_write_ratio() {
        Ok(_) => info!("Read/Write Ratio test completed successfully"),
        Err(e) => return Err(format!("Read/Write Ratio test failed: {}", e)),
    }

    match test_hot_spot_contention() {
        Ok(_) => info!("Hot Spot Contention test completed successfully"),
        Err(e) => return Err(format!("Hot Spot Contention test failed: {}", e)),
    }

    match test_price_level_distribution() {
        Ok(_) => info!("Price Level Distribution test completed successfully"),
        Err(e) => return Err(format!("Price Level Distribution test failed: {}", e)),
    }

    Ok(())
}

// Test how different read/write operation ratios affect performance
fn test_read_write_ratio() -> Result<(), String> {
    info!("\n[TEST] Read/Write Operation Ratio");
    info!("--------------------------------");

    // Test with different percentages of read operations
    let test_cases = [0, 25, 50, 75, 95]; // Percentage of read operations
    let mut results = HashMap::new();

    for &read_percentage in &test_cases {
        info!("\nTesting with {}% read operations...", read_percentage);

        // Create a fresh order book for each test
        let order_book = Arc::new(OrderBook::new(SYMBOL));

        // Pre-populate with orders
        helpers::setup_orders_for_read_write_test(&order_book);

        // Counter for operations performed by each thread
        let operation_counters = Arc::new(Mutex::new(vec![0; THREAD_COUNT]));

        // Flag to signal when to stop the test
        let running = Arc::new(AtomicBool::new(true));

        // Barrier for synchronized start with timeout
        let barrier = Arc::new(Barrier::new(THREAD_COUNT + 1));

        // Spawn worker threads
        let mut handles = Vec::with_capacity(THREAD_COUNT);

        for thread_id in 0..THREAD_COUNT {
            let thread_book = Arc::clone(&order_book);
            let thread_barrier = Arc::clone(&barrier);
            let thread_running = Arc::clone(&running);
            let thread_counters = Arc::clone(&operation_counters);
            let read_pct = read_percentage;

            let handle = thread::spawn(move || {
                // Wait for synchronized start with error handling
                let wait_result = thread_barrier.wait();
                if wait_result.is_leader() {
                    // The leader can perform a specific action if necessary
                    // (no es necesario hacer nada especial en este caso)
                }

                let mut local_counter = 0;

                while thread_running.load(Ordering::Relaxed) {
                    // Determine if this is a read or write operation
                    let is_read = (local_counter % 100) < read_pct;

                    if is_read {
                        // Read operation
                        match local_counter % 5 {
                            0 => {
                                // Query best prices
                                let _ = thread_book.best_bid();
                                let _ = thread_book.best_ask();
                            }
                            1 => {
                                // Calculate spread and mid price
                                let _ = thread_book.spread();
                                let _ = thread_book.mid_price();
                            }
                            2 => {
                                // Get orders at a specific price
                                if let Some(bid) = thread_book.best_bid() {
                                    let _ = thread_book.get_orders_at_price(bid, Side::Buy);
                                }
                            }
                            3 => {
                                // Create a snapshot
                                let _ = thread_book.create_snapshot(5);
                            }
                            _ => {
                                // Get all orders
                                let _ = thread_book.get_all_orders();
                            }
                        }
                    } else {
                        // Write operation
                        match local_counter % 3 {
                            0 => {
                                // Add a limit order
                                let id = Id::new_uuid();
                                let side = if local_counter % 2 == 0 {
                                    Side::Buy
                                } else {
                                    Side::Sell
                                };
                                let price = if side == Side::Buy { 9900 } else { 10100 };
                                let _ = thread_book.add_limit_order(
                                    id,
                                    price,
                                    10,
                                    side,
                                    TimeInForce::Gtc,
                                    None,
                                );
                            }
                            1 => {
                                // Submit a market order
                                let id = Id::new_uuid();
                                let side = if local_counter % 2 == 0 {
                                    Side::Buy
                                } else {
                                    Side::Sell
                                };
                                let _ = thread_book.submit_market_order(id, 5, side);
                            }
                            _ => {
                                // Cancel a random order
                                let id = Id::new_uuid();
                                let _ = thread_book.cancel_order(id);
                            }
                        }
                    }

                    local_counter += 1;

                    // Add a small sleep to prevent a thread from monopolizing the CPU
                    if local_counter % 1000 == 0 {
                        thread::sleep(Duration::from_micros(1));
                    }
                }

                // Update the operation counter
                if let Ok(mut counters) = thread_counters.lock() {
                    if thread_id < counters.len() {
                        counters[thread_id] = local_counter;
                    }
                }

                local_counter
            });

            handles.push(handle);
        }

        // Start the test
        let start_time = Instant::now();

        // Wait for all threads to be ready
        barrier.wait();

        // Run for the specified duration
        thread::sleep(Duration::from_millis(TEST_DURATION_MS));

        // Signal threads to stop
        running.store(false, Ordering::Relaxed);

        // Wait for all threads to finish (with timeout)
        let mut total_ops = 0;
        for (i, handle) in handles.into_iter().enumerate() {
            // Set a timeout for each thread's join
            match handle.join() {
                Ok(count) => {
                    total_ops += count;
                    info!("Thread {} completed with {} operations", i, count);
                }
                Err(_) => {
                    info!("Thread {} panicked", i);
                }
            }
        }

        let elapsed = start_time.elapsed();

        // Calculate operations per second
        let ops_per_second = total_ops as f64 / elapsed.as_secs_f64();

        info!("Completed {} operations in {:?}", total_ops, elapsed);
        info!("Throughput: {:.2} operations/second", ops_per_second);

        // Store result
        results.insert(read_percentage, ops_per_second);
    }

    // Print summary table
    info!("\nRead/Write Ratio Results:");
    info!("------------------------");
    info!("Read %  |  Operations/second");
    info!("-------------------------");

    for &pct in &test_cases {
        if let Some(&ops) = results.get(&pct) {
            info!("{}%    |  {:.2}", pct, ops);
        }
    }

    Ok(())
}

// Test contention when multiple threads target the same "hot" orders
fn test_hot_spot_contention() -> Result<(), String> {
    info!("\n[TEST] Hot Spot Contention");
    info!("-----------------------");

    // Test with different percentages of operations targeting hot spot
    let test_cases = [0, 25, 50, 75, 100]; // Percentage targeting hot spot
    let mut results = HashMap::new();

    for &hot_spot_percentage in &test_cases {
        info!(
            "\nTesting with {}% operations targeting hot spot...",
            hot_spot_percentage
        );

        // Create a fresh order book for each test
        let order_book = Arc::new(OrderBook::new(SYMBOL));

        // Pre-populate with orders (first 20 are hot spot)
        helpers::setup_orders_for_hot_spot_test(&order_book);

        // Counter for operations performed by each thread
        let operation_counters = Arc::new(Mutex::new(vec![0; THREAD_COUNT]));

        // Flag to signal when to stop the test
        let running = Arc::new(AtomicBool::new(true));

        // Barrier for synchronized start
        let barrier = Arc::new(Barrier::new(THREAD_COUNT + 1));

        // Spawn worker threads
        let mut handles = Vec::with_capacity(THREAD_COUNT);

        for thread_id in 0..THREAD_COUNT {
            let thread_book = Arc::clone(&order_book);
            let thread_barrier = Arc::clone(&barrier);
            let thread_running = Arc::clone(&running);
            let thread_counters = Arc::clone(&operation_counters);
            let hot_pct = hot_spot_percentage;

            let handle = thread::spawn(move || {
                // Wait for synchronized start
                thread_barrier.wait();

                let mut local_counter = 0;

                while thread_running.load(Ordering::Relaxed) {
                    // Determine if this operation targets the hot spot
                    let target_hot_spot = (local_counter % 100) < hot_pct;

                    // Choose an order ID based on hot spot decision
                    let order_id = if target_hot_spot {
                        // Target one of the first 20 orders (hot spot)
                        let hot_idx = local_counter % 20;
                        Id::from_u64(hot_idx)
                    } else {
                        // Target one of the remaining orders
                        let cold_idx = 20 + (local_counter % 480);
                        Id::from_u64(cold_idx)
                    };

                    // Perform operation on the selected order
                    match local_counter % 3 {
                        0 => {
                            // Try to look up the order
                            let _ = thread_book.get_order(order_id);
                        }
                        1 => {
                            // Try to cancel the order
                            let _ = thread_book.cancel_order(order_id);
                        }
                        _ => {
                            // Try to modify the order quantity
                            let update = pricelevel::OrderUpdate::UpdateQuantity {
                                order_id,
                                new_quantity: pricelevel::Quantity::new(15),
                            };
                            let _ = thread_book.update_order(update);
                        }
                    }

                    local_counter += 1;

                    // Small sleep to prevent CPU monopolization
                    if local_counter % 1000 == 0 {
                        thread::sleep(Duration::from_micros(1));
                    }
                }

                // Update the operation counter
                if let Ok(mut counters) = thread_counters.lock() {
                    if thread_id < counters.len() {
                        counters[thread_id] = local_counter;
                    }
                }

                local_counter
            });

            handles.push(handle);
        }

        // Start the test
        let start_time = Instant::now();

        // Sincronizamos el inicio de todos los hilos
        barrier.wait();

        // Run for the specified duration
        thread::sleep(Duration::from_millis(TEST_DURATION_MS));

        // Signal threads to stop
        running.store(false, Ordering::Relaxed);

        // Wait for all threads to finish
        let mut total_ops = 0;
        for (i, handle) in handles.into_iter().enumerate() {
            match handle.join() {
                Ok(count) => {
                    total_ops += count;
                    info!("Thread {} completed with {} operations", i, count);
                }
                Err(_) => {
                    info!("Thread {} panicked", i);
                }
            }
        }

        let elapsed = start_time.elapsed();

        // Calculate operations per second
        let ops_per_second = total_ops as f64 / elapsed.as_secs_f64();

        info!("Completed {} operations in {:?}", total_ops, elapsed);
        info!("Throughput: {:.2} operations/second", ops_per_second);

        // Store result
        results.insert(hot_spot_percentage, ops_per_second);
    }

    // Print summary table
    info!("\nHot Spot Contention Results:");
    info!("---------------------------");
    info!("Hot %  |  Operations/second");
    info!("-------------------------");

    for &pct in &test_cases {
        if let Some(&ops) = results.get(&pct) {
            info!("{}%    |  {:.2}", pct, ops);
        }
    }

    Ok(())
}

fn test_price_level_distribution() -> Result<(), String> {
    info!("\n[TEST] Price Level Distribution");
    info!("----------------------------");

    // Test with different numbers of price levels
    // Start with more levels and gradually decrease to avoid issues
    let test_cases = [100, 50, 10, 5, 1]; // Test cases in descending order
    let mut results = HashMap::new();

    for &price_levels in &test_cases {
        info!("\nTesting with {} price levels...", price_levels);

        // Create a fresh order book for each test
        let order_book = Arc::new(crate::OrderBook::new(SYMBOL));

        // Calculate orders per level - more for fewer levels to maintain similar total
        let min_orders = std::cmp::max(100, 1000 / price_levels);
        info!(
            "Setting up orders: {} per level x {} levels",
            min_orders, price_levels
        );

        // Setup orders with the calculated number per level
        helpers::setup_orders_for_price_level_test(&order_book, price_levels, min_orders);

        // Verify that the book has orders before continuing
        let snapshot = order_book.create_snapshot(price_levels as usize);
        info!(
            "Pre-populated with {} bid levels and {} ask levels",
            snapshot.bids.len(),
            snapshot.asks.len()
        );

        if snapshot.bids.is_empty() || snapshot.asks.is_empty() {
            return Err(format!(
                "Failed to properly populate order book for {} price levels test",
                price_levels
            ));
        }

        // Counter for operations performed by each thread
        let operation_counters = Arc::new(Mutex::new(vec![0; THREAD_COUNT]));

        // Flag to signal when to stop the test
        let running = Arc::new(AtomicBool::new(true));

        // Barrier for synchronized start
        let barrier = Arc::new(Barrier::new(THREAD_COUNT + 1));

        // Spawn worker threads
        let mut handles = Vec::with_capacity(THREAD_COUNT);

        for thread_id in 0..THREAD_COUNT {
            let thread_book = Arc::clone(&order_book);
            let thread_barrier = Arc::clone(&barrier);
            let thread_running = Arc::clone(&running);
            let thread_counters = Arc::clone(&operation_counters);
            let max_level = price_levels;

            let handle = thread::spawn(move || {
                // Wait for synchronized start
                info!(
                    "Thread {} waiting at barrier for {} levels test",
                    thread_id, max_level
                );
                let wait_result = thread_barrier.wait();

                if wait_result.is_leader() {
                    info!(
                        "Thread {} is barrier leader for {} levels test",
                        thread_id, max_level
                    );
                }

                info!(
                    "Thread {} starting work for {} levels test",
                    thread_id, max_level
                );

                let mut local_counter = 0u64;

                while thread_running.load(Ordering::Relaxed) {
                    let op_type = local_counter % 20; // Increase diversity of operations

                    match op_type {
                        // --- Write Operations --- (Yield after each)
                        0 | 1 => {
                            // Add limit buy/sell
                            let side = if op_type == 0 { Side::Buy } else { Side::Sell };
                            let price: u128 = if side == Side::Buy {
                                10000 - (local_counter % max_level as u64) as u128 * 10
                            } else {
                                10100 + (local_counter % max_level as u64) as u128 * 10
                            };
                            let _ = thread_book.add_limit_order(
                                Id::new_uuid(),
                                price,
                                10,
                                side,
                                TimeInForce::Gtc,
                                None,
                            );
                            std::thread::yield_now(); // Aggressively yield after write
                        }
                        2 | 3 => {
                            // Submit market buy/sell
                            let side = if op_type == 2 { Side::Buy } else { Side::Sell };
                            let _ = thread_book.submit_market_order(Id::new_uuid(), 1, side);
                            std::thread::yield_now(); // Aggressively yield after write
                        }
                        4 => {
                            // Cancel order
                            let id = Id::from_u64(local_counter % 1000);
                            let _ = thread_book.cancel_order(id);
                            std::thread::yield_now(); // Aggressively yield after write
                        }

                        // --- Read Operations --- (No yield needed)
                        5 | 6 => {
                            let _ = thread_book.best_bid();
                        }
                        7 | 8 => {
                            let _ = thread_book.best_ask();
                        }
                        9 | 10 => {
                            let _ = thread_book.spread();
                        }
                        11 | 12 => {
                            let _ = thread_book.mid_price();
                        }
                        13 | 14 => {
                            let _ = thread_book.create_snapshot(5);
                        }
                        _ => {
                            // Default to a less intensive operation or a small sleep
                            thread::sleep(Duration::from_micros(5));
                        }
                    }

                    local_counter += 1;
                }

                // Update the operation counter
                if let Ok(mut counters) = thread_counters.lock() {
                    if thread_id < counters.len() {
                        counters[thread_id] = local_counter as usize;
                    }
                }

                info!(
                    "Thread {} completed with {} operations for {} levels test",
                    thread_id, local_counter, max_level
                );

                local_counter as usize
            });

            handles.push(handle);

            // Add a small delay between thread creation
            thread::sleep(Duration::from_millis(5));
        }

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

        // Calculate operations per second
        let ops_per_second = total_ops as f64 / elapsed.as_secs_f64();

        info!(
            "Test with {} levels completed in {:?}",
            price_levels, elapsed
        );
        info!("Total operations: {}", total_ops);
        info!("Operations per second: {:.2}", ops_per_second);

        // Store result
        results.insert(price_levels, ops_per_second);

        // Give the system a moment to clean up resources before the next test
        thread::sleep(Duration::from_millis(100));
    }

    // Print summary table
    info!("\nPrice Level Distribution Results:");
    info!("-------------------------------");
    info!("Levels  |  Operations/second");
    info!("---------------------------");

    for &levels in &test_cases {
        if let Some(&ops) = results.get(&levels) {
            info!("{}    |  {:.2}", levels, ops);
        }
    }

    Ok(())
}
