use criterion::{BenchmarkId, Criterion};
use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce};
use std::hint::black_box;

/// Register all benchmarks for matching orders in an order book
pub fn register_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Match Orders");
    group.sample_size(100); // Adjust sample size for more consistent results

    // Benchmark market order against limit orders
    group.bench_function("match_market_against_limit", |b| {
        b.iter(|| {
            let order_book = setup_limit_order_book(100);
            let id = Id::new_uuid();
            let _ = black_box(order_book.submit_market_order(id, 50, Side::Buy));
        })
    });

    // Benchmark market order against iceberg orders
    group.bench_function("match_market_against_iceberg", |b| {
        b.iter(|| {
            let order_book = setup_iceberg_order_book(100);
            let id = Id::new_uuid();
            let _ = black_box(order_book.submit_market_order(id, 75, Side::Buy));
        })
    });

    // Benchmark with different match quantities against limit orders
    for match_quantity in [10, 50, 100, 200, 500].iter() {
        group.bench_with_input(
            BenchmarkId::new("match_quantity_scaling", match_quantity),
            match_quantity,
            |b, &match_quantity| {
                b.iter(|| {
                    let order_book = setup_limit_order_book(50);
                    let id = Id::new_uuid();
                    let _ =
                        black_box(order_book.submit_market_order(id, match_quantity, Side::Buy));
                })
            },
        );
    }

    group.finish();
}

// Helper function to set up an order book with limit orders
fn setup_limit_order_book(order_count: u64) -> OrderBook {
    let order_book = OrderBook::new("TEST-SYMBOL");

    for _i in 0..order_count {
        let id = Id::new_uuid();
        order_book
            .add_limit_order(id, 1000, 10, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();
    }

    order_book
}

// Helper function to set up an order book with iceberg orders
fn setup_iceberg_order_book(order_count: u64) -> OrderBook {
    let order_book = OrderBook::new("TEST-SYMBOL");

    for _i in 0..order_count {
        let id = Id::new_uuid();
        order_book
            .add_iceberg_order(id, 1000, 5, 15, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();
    }

    order_book
}
