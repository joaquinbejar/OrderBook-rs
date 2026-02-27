use criterion::{BenchmarkId, Criterion, criterion_group};
use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

pub fn register_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Concurrent Operations");

    // Test with various thread counts
    for thread_count in [2, 4, 8, 16].iter() {
        group.bench_with_input(
            BenchmarkId::new("concurrent_add_limit_orders", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter_custom(|iters| {
                    measure_concurrent_operation(
                        thread_count,
                        iters,
                        |order_book, _thread_id, _iteration| {
                            // Each thread adds orders with unique IDs
                            let id = Id::new_uuid();
                            order_book
                                .add_limit_order(id, 1000, 10, Side::Buy, TimeInForce::Gtc, None)
                                .unwrap();
                        },
                    )
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("concurrent_mixed_operations", thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter_custom(|iters| measure_concurrent_mixed_operations(thread_count, iters));
            },
        );
    }

    group.finish();
}

/// Measures time for concurrent operations on an order book
fn measure_concurrent_operation<F>(thread_count: usize, iterations: u64, operation: F) -> Duration
where
    F: Fn(&Arc<OrderBook>, usize, u64) + Send + Sync + 'static,
{
    let order_book = Arc::new(OrderBook::new("TEST-SYMBOL"));
    let operation = Arc::new(operation);
    let barrier = Arc::new(Barrier::new(thread_count + 1)); // +1 for main thread

    let mut handles = Vec::with_capacity(thread_count);

    for thread_id in 0..thread_count {
        let thread_order_book = Arc::clone(&order_book);
        let thread_barrier = Arc::clone(&barrier);
        let thread_operation = Arc::clone(&operation);

        handles.push(thread::spawn(move || {
            // Wait for all threads to be ready
            thread_barrier.wait();

            for i in 0..iterations {
                thread_operation(&thread_order_book, thread_id, i);
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

/// Measures time for mixed concurrent operations (add, match, cancel) on an order book
fn measure_concurrent_mixed_operations(thread_count: usize, iterations: u64) -> Duration {
    let order_book: Arc<OrderBook> = Arc::new(OrderBook::new("TEST-SYMBOL"));
    let barrier = Arc::new(Barrier::new(thread_count + 1)); // +1 for main thread

    // Pre-populate with some orders
    for i in 0..200 {
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
                // Determine operation based on iteration
                match i % 4 {
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
                        // Match with a market order
                        let id = Id::new_uuid();
                        let side = if thread_id % 2 == 0 {
                            Side::Buy
                        } else {
                            Side::Sell
                        };
                        thread_order_book.submit_market_order(id, 5, side).ok();
                    }
                    2 => {
                        // Get all orders and maybe cancel some
                        if let Some(order) = thread_order_book.get_all_orders().get(thread_id % 10)
                        {
                            thread_order_book.cancel_order(order.id()).ok();
                        }
                    }
                    _ => {
                        // Create a snapshot
                        thread_order_book.create_snapshot(5);
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

criterion_group!(concurrent_benches, register_benchmarks);
