use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
use tracing::info;

// Number of threads to use for the test
const THREAD_COUNT: usize = 8;
// Duration of the test in seconds
const TEST_DURATION_SECS: u64 = 5;

fn main() {
    // Set up logging
    setup_logger();
    info!("Multi-threaded OrderBook Performance Test");
    info!("----------------------------------------");
    info!("Threads: {}", THREAD_COUNT);
    info!("Duration: {} seconds", TEST_DURATION_SECS);

    // Run the multi-threaded test
    run_performance_test();
}

fn run_performance_test() {
    // Create a shared OrderBook
    let book = Arc::new(OrderBook::new("PERF-TEST"));

    // Pre-populate with orders to have a realistic test scenario
    populate_orderbook(&book, 1000);

    // Create thread performance counters
    let mut operation_counters = vec![0; THREAD_COUNT];

    // Synchronization barrier to ensure all threads start at the same time
    let barrier = Arc::new(Barrier::new(THREAD_COUNT + 1)); // +1 for main thread

    // Flag to signal threads to stop
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));

    // Spawn worker threads
    let mut handles = Vec::with_capacity(THREAD_COUNT);

    for thread_id in 0..THREAD_COUNT {
        let thread_book = Arc::clone(&book);
        let thread_barrier = Arc::clone(&barrier);
        let thread_running = Arc::clone(&running);

        let handle = thread::spawn(move || {
            // Wait for all threads to be ready
            thread_barrier.wait();

            let mut local_counter: u64 = 0;

            // Run operations until the main thread signals to stop
            while thread_running.load(std::sync::atomic::Ordering::Relaxed) {
                // Perform operations based on thread ID to simulate different workloads
                match thread_id % 4 {
                    0 => {
                        // This thread adds limit orders
                        let buy_side = local_counter % 2 == 0;
                        let side = if buy_side { Side::Buy } else { Side::Sell };
                        let price_base: u128 = if buy_side { 9900 } else { 10100 };
                        let price_offset: u128 = (local_counter as u128 % 10) * 10;
                        let price: u128 = price_base + price_offset;

                        let id = Id::new_uuid();
                        let _ = thread_book.add_limit_order(
                            id,
                            price,
                            10, // quantity
                            side,
                            TimeInForce::Gtc,
                            None,
                        );
                    }
                    1 => {
                        // This thread submits market orders
                        let side = if local_counter % 2 == 0 {
                            Side::Buy
                        } else {
                            Side::Sell
                        };
                        let quantity = 5 + (local_counter % 5); // 5-9 units

                        let id = Id::new_uuid();
                        let _ = thread_book.submit_market_order(id, quantity, side);
                    }
                    2 => {
                        // This thread cancels orders
                        // Use order IDs that have a chance of existing but will often miss
                        // (this simulates a realistic scenario with many cancellations failing)
                        let target_id = Id::new_uuid();
                        let _ = thread_book.cancel_order(target_id);
                    }
                    3 => {
                        // This thread queries the order book
                        match local_counter % 5 {
                            0 => {
                                let _ = thread_book.best_bid();
                            }
                            1 => {
                                let _ = thread_book.best_ask();
                            }
                            2 => {
                                let _ = thread_book.spread();
                            }
                            3 => {
                                if let Some(best_bid) = thread_book.best_bid() {
                                    let _ = thread_book.get_orders_at_price(best_bid, Side::Buy);
                                }
                            }
                            _ => {
                                let _ = thread_book.create_snapshot(5);
                            }
                        }
                    }
                    _ => unreachable!(),
                }

                local_counter += 1;
            }

            // Return the number of operations performed
            local_counter
        });

        handles.push(handle);
    }

    // Start the test
    info!("Starting performance test...");
    let start_time = Instant::now();

    // Release all threads to start working
    barrier.wait();

    // Run the test for the specified duration
    thread::sleep(Duration::from_secs(TEST_DURATION_SECS));

    // Signal threads to stop
    running.store(false, std::sync::atomic::Ordering::Relaxed);

    // Wait for all threads to finish and collect their operation counts
    for (i, handle) in handles.into_iter().enumerate() {
        match handle.join() {
            Ok(count) => {
                operation_counters[i] = count;
            }
            Err(_) => {
                info!("Thread {} panicked", i);
            }
        }
    }

    let elapsed = start_time.elapsed();
    info!("Performance test completed in {:?}", elapsed);

    // Calculate total operations and operations per second
    let total_operations: u64 = operation_counters.iter().sum();
    let operations_per_sec = total_operations as f64 / elapsed.as_secs_f64();

    // Print performance results
    info!("\nPerformance Results:");
    info!("-------------------");
    info!("Total operations: {}", total_operations);
    info!("Operations per second: {:.2}", operations_per_sec);

    // Print per-thread statistics
    info!("\nPer-thread Operations:");
    for (i, &count) in operation_counters.iter().enumerate() {
        let thread_type = match i % 4 {
            0 => "Limit Order Adder",
            1 => "Market Order Submitter",
            2 => "Order Canceller",
            3 => "Order Book Querier",
            _ => unreachable!(),
        };

        info!("Thread {} ({}): {} operations", i, thread_type, count);
    }

    // Print order book state after the test
    print_orderbook_state(&book);
}

fn populate_orderbook(book: &OrderBook, order_count: usize) {
    info!(
        "Populating OrderBook with {} initial orders...",
        order_count
    );

    // Add buy orders
    for i in 0..(order_count / 2) {
        let price: u128 = 9900 + (i as u128 % 100) * 10; // 9900-10890
        let id = Id::new_uuid();
        let _ = book.add_limit_order(
            id,
            price,
            (10 + (i % 10)) as u64, // quantity 10-19
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );
    }

    // Add sell orders
    for i in 0..(order_count / 2) {
        let price: u128 = 10100 + (i as u128 % 100) * 10; // 10100-11090
        let id = Id::new_uuid();
        let _ = book.add_limit_order(
            id,
            price,
            (10 + (i % 10)) as u64, // quantity 10-19
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
    }

    // Add some iceberg orders for good measure
    for i in 0..20 {
        let is_buy = i % 2 == 0;
        let side = if is_buy { Side::Buy } else { Side::Sell };
        let price_base = if is_buy { 9950 } else { 10050 };
        let price = price_base + (i * 5);

        let id = Id::new_uuid();
        let _ = book.add_iceberg_order(
            id,
            price,
            5,  // visible quantity
            50, // hidden quantity
            side,
            TimeInForce::Gtc,
            None,
        );
    }

    info!("OrderBook populated successfully.");
}

fn print_orderbook_state(book: &OrderBook) {
    info!("\nOrderBook State After Test:");
    info!("---------------------------");

    // Book summary
    info!("Symbol: {}", book.symbol());

    // Best prices
    match (book.best_bid(), book.best_ask()) {
        (Some(bid), Some(ask)) => {
            info!("Best bid: {}", bid);
            info!("Best ask: {}", ask);
            info!("Spread: {}", ask - bid);
            if let Some(mid) = book.mid_price() {
                info!("Mid price: {:.2}", mid);
            }
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

    // Order counts
    let all_orders = book.get_all_orders();
    info!("Total orders in book: {}", all_orders.len());

    // Last trade price
    if let Some(last_trade) = book.last_trade_price() {
        info!("Last trade price: {}", last_trade);
    }

    // Volume by price
    let (bid_volumes, ask_volumes) = book.get_volume_by_price();
    info!("Number of bid price levels: {}", bid_volumes.len());
    info!("Number of ask price levels: {}", ask_volumes.len());

    // Top bid/ask levels
    let snapshot = book.create_snapshot(3);

    info!("\nTop Bid Levels:");
    for level in snapshot.bids {
        info!(
            "Price: {}, Quantity: {} (visible: {}, hidden: {}), Orders: {}",
            level.price(),
            level.visible_quantity() + level.hidden_quantity(),
            level.visible_quantity(),
            level.hidden_quantity(),
            level.order_count()
        );
    }

    info!("\nTop Ask Levels:");
    for level in snapshot.asks {
        info!(
            "Price: {}, Quantity: {} (visible: {}, hidden: {}), Orders: {}",
            level.price(),
            level.visible_quantity() + level.hidden_quantity(),
            level.visible_quantity(),
            level.hidden_quantity(),
            level.order_count()
        );
    }
}
