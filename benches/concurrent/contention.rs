use criterion::{BenchmarkId, Criterion};
use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

/// Register benchmarks that test different contention patterns
#[allow(dead_code)]
pub fn register_contention_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Contention Patterns");

    // Test with different read/write ratios
    for read_ratio in [0, 25, 50, 75, 95].iter() {
        // Fixed at 8 threads which is a common server core count
        let thread_count = 8;

        group.bench_with_input(
            BenchmarkId::new("read_write_ratio", read_ratio),
            read_ratio,
            |b, &read_ratio| {
                b.iter_custom(|iters| {
                    measure_read_write_contention(thread_count, iters, read_ratio)
                });
            },
        );
    }

    // Test with different access patterns (hot spot vs distributed)
    for hot_spot_percentage in [0, 20, 50, 80, 100].iter() {
        // Fixed at 8 threads
        let thread_count = 8;

        group.bench_with_input(
            BenchmarkId::new("hot_spot_contention", hot_spot_percentage),
            hot_spot_percentage,
            |b, &hot_spot_percentage| {
                b.iter_custom(|iters| {
                    measure_hot_spot_contention(thread_count, iters, hot_spot_percentage)
                });
            },
        );
    }

    group.finish();
}

/// Measures time for operations with different read/write ratios
/// read_ratio = percentage of read operations (0-100)
#[allow(dead_code)]
fn measure_read_write_contention(
    thread_count: usize,
    iterations: u64,
    read_ratio: usize,
) -> Duration {
    let order_book: Arc<OrderBook> = Arc::new(OrderBook::new("TEST-SYMBOL"));
    let barrier = Arc::new(Barrier::new(thread_count + 1)); // +1 for main thread

    // Pre-populate with orders to read against
    for i in 0..500 {
        let id = Id::new_uuid();
        let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
        let price = if side == Side::Buy { 990 } else { 1010 };
        order_book
            .add_limit_order(id, price, 10, side, TimeInForce::Gtc, None)
            .unwrap();
    }

    let mut handles = Vec::with_capacity(thread_count);

    for thread_id in 0..thread_count {
        let thread_order_book = Arc::clone(&order_book);
        let thread_barrier = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            // Wait for all threads to be ready
            thread_barrier.wait();

            for i in 0..iterations {
                // Determine if this is a read or write operation
                let is_read = (i as usize % 100) < read_ratio;

                if is_read {
                    // Read operation: snapshot or query
                    if i % 2 == 0 {
                        // Create a snapshot (read-only)
                        let _ = thread_order_book.create_snapshot(5);
                    } else {
                        // Other read operations
                        let _ = thread_order_book.get_all_orders();
                        let _ = thread_order_book.best_bid();
                        let _ = thread_order_book.best_ask();
                    }
                } else {
                    // Write operation
                    let op_type = i % 3;

                    match op_type {
                        0 => {
                            // Add a new order
                            let id = Id::new_uuid();
                            let side = if thread_id % 2 == 0 {
                                Side::Buy
                            } else {
                                Side::Sell
                            };
                            let price = if side == Side::Buy { 990 } else { 1010 };
                            thread_order_book
                                .add_limit_order(id, price, 10, side, TimeInForce::Gtc, None)
                                .unwrap();
                        }
                        1 => {
                            // Submit a market order
                            let id = Id::new_uuid();
                            let side = if thread_id % 2 == 0 {
                                Side::Buy
                            } else {
                                Side::Sell
                            };
                            thread_order_book.submit_market_order(id, 2, side).ok();
                        }
                        _ => {
                            // Cancel an order if we can find one
                            if let Some(order) =
                                thread_order_book.get_all_orders().get(thread_id % 10)
                            {
                                thread_order_book.cancel_order(order.id()).ok();
                            }
                        }
                    }
                }
            }

            // Signal completion
            thread_barrier.wait();
        }));
    }

    // Start timing
    barrier.wait();
    let start = Instant::now();

    // Wait for all threads to complete
    barrier.wait();
    let duration = start.elapsed();

    // Join all threads
    for handle in handles {
        let _ = handle.join();
    }

    duration
}

/// Measures time for operations with different hot spot patterns
/// hot_spot_percentage = percentage of operations targeting the same hot spot price level (0-100)
#[allow(dead_code)]
fn measure_hot_spot_contention(
    thread_count: usize,
    iterations: u64,
    hot_spot_percentage: usize,
) -> Duration {
    let order_book: Arc<OrderBook> = Arc::new(OrderBook::new("TEST-SYMBOL"));
    let barrier = Arc::new(Barrier::new(thread_count + 1)); // +1 for main thread

    // Create "hot spot" price level at 1000
    for _i in 0..20 {
        let id = Id::new_uuid();
        order_book
            .add_limit_order(id, 1000, 10, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();
    }

    // Create other price levels from 1001-1020
    for i in 1..20 {
        let id = Id::new_uuid();
        order_book
            .add_limit_order(id, 1000 + i, 10, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();
    }

    let mut handles = Vec::with_capacity(thread_count);

    for thread_id in 0..thread_count {
        let thread_order_book = Arc::clone(&order_book);
        let thread_barrier = Arc::clone(&barrier);

        handles.push(thread::spawn(move || {
            // Wait for all threads to be ready
            thread_barrier.wait();

            for i in 0..iterations {
                // Determine if this operation targets the hot spot
                let target_hot_spot = (i as usize % 100) < hot_spot_percentage;

                // Choose price based on hot spot decision
                let price: u128 = if target_hot_spot {
                    1000 // Hot spot price
                } else {
                    1001 + (thread_id % 19) as u128 // Other prices
                };

                // Perform operations
                let op_type = i % 3;
                match op_type {
                    0 => {
                        // Add a new order at selected price
                        let id = Id::new_uuid();
                        thread_order_book
                            .add_limit_order(id, price, 10, Side::Buy, TimeInForce::Gtc, None)
                            .unwrap();
                    }
                    1 => {
                        // Get orders at the price level
                        let _ = thread_order_book.get_orders_at_price(price, Side::Sell);
                    }
                    _ => {
                        // Submit a market order (will match against orders at the price)
                        let id = Id::new_uuid();
                        thread_order_book.submit_market_order(id, 1, Side::Buy).ok();
                    }
                }
            }

            // Signal completion
            thread_barrier.wait();
        }));
    }

    // Start timing
    barrier.wait();
    let start = Instant::now();

    // Wait for all threads to complete
    barrier.wait();
    let duration = start.elapsed();

    // Join all threads
    for handle in handles {
        let _ = handle.join();
    }

    duration
}
