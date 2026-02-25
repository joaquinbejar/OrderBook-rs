use criterion::{BenchmarkId, Criterion};
use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce};
use std::hint::black_box;

/// Register all benchmarks for adding orders to an order book
pub fn register_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Add Orders");

    // Benchmark adding limit orders
    group.bench_function("add_limit_orders", |b| {
        b.iter(|| {
            let order_book: OrderBook = OrderBook::new("TEST-SYMBOL");
            for i in 0..100 {
                let id = Id::new_uuid();
                let _ = black_box(order_book.add_limit_order(
                    id,
                    1000 + i,
                    10,
                    Side::Buy,
                    TimeInForce::Gtc,
                    None,
                ));
            }
        })
    });

    // Benchmark adding iceberg orders
    group.bench_function("add_iceberg_orders", |b| {
        b.iter(|| {
            let order_book: OrderBook = OrderBook::new("TEST-SYMBOL");
            for i in 0..100 {
                let id = Id::new_uuid();
                let _ = black_box(order_book.add_iceberg_order(
                    id,
                    1000 + i,
                    5,
                    15,
                    Side::Sell,
                    TimeInForce::Gtc,
                    None,
                ));
            }
        })
    });

    // Benchmark adding post-only orders
    group.bench_function("add_post_only_orders", |b| {
        b.iter(|| {
            let order_book: OrderBook = OrderBook::new("TEST-SYMBOL");
            for i in 0..100 {
                let id = Id::new_uuid();
                let _ = black_box(order_book.add_post_only_order(
                    id,
                    1000 + i,
                    10,
                    Side::Buy,
                    TimeInForce::Gtc,
                    None,
                ));
            }
        })
    });

    // Parametrized benchmark with different order counts
    for order_count in [10, 100, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::new("order_count_scaling", order_count),
            order_count,
            |b, &order_count| {
                b.iter(|| {
                    let order_book: OrderBook = OrderBook::new("TEST-SYMBOL");
                    for _i in 0..order_count {
                        let id = Id::new_uuid();
                        let _ = black_box(order_book.add_limit_order(
                            id,
                            1000,
                            10,
                            Side::Buy,
                            TimeInForce::Gtc,
                            None,
                        ));
                    }
                })
            },
        );
    }

    group.finish();
}
