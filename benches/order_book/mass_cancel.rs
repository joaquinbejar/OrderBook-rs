use criterion::{BenchmarkId, Criterion};
use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce};
use std::hint::black_box;

/// Register all benchmarks for mass cancel operations.
pub fn register_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Mass Cancel");

    // Benchmark cancel_all_orders with varying order counts
    for &order_count in &[100, 1_000, 10_000, 50_000] {
        group.bench_with_input(
            BenchmarkId::new("cancel_all_orders", order_count),
            &order_count,
            |b, &count| {
                b.iter_with_setup(
                    || {
                        let book: OrderBook<()> = OrderBook::new("BENCH");
                        // Populate the book: half bids, half asks across price levels
                        for i in 0..count {
                            let id = Id::new_uuid();
                            let price = 1000 + (i % 500) as u128;
                            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
                            let _ =
                                book.add_limit_order(id, price, 10, side, TimeInForce::Gtc, None);
                        }
                        book
                    },
                    |book| {
                        let result = black_box(book.cancel_all_orders());
                        assert_eq!(result.cancelled_count(), count);
                    },
                );
            },
        );
    }

    // Benchmark cancel_orders_by_side
    for &order_count in &[100, 1_000, 10_000] {
        group.bench_with_input(
            BenchmarkId::new("cancel_orders_by_side", order_count),
            &order_count,
            |b, &count| {
                b.iter_with_setup(
                    || {
                        let book: OrderBook<()> = OrderBook::new("BENCH");
                        for i in 0..count {
                            let id = Id::new_uuid();
                            let price = 1000 + (i % 500) as u128;
                            let _ = book.add_limit_order(
                                id,
                                price,
                                10,
                                Side::Buy,
                                TimeInForce::Gtc,
                                None,
                            );
                        }
                        book
                    },
                    |book| {
                        let result = black_box(book.cancel_orders_by_side(Side::Buy));
                        assert_eq!(result.cancelled_count(), count);
                    },
                );
            },
        );
    }

    // Benchmark cancel_orders_by_user
    for &order_count in &[100, 1_000, 10_000] {
        group.bench_with_input(
            BenchmarkId::new("cancel_orders_by_user", order_count),
            &order_count,
            |b, &count| {
                b.iter_with_setup(
                    || {
                        let book: OrderBook<()> = OrderBook::new("BENCH");
                        let user = pricelevel::Hash32::new([1u8; 32]);
                        for i in 0..count {
                            let id = Id::new_uuid();
                            let price = 1000 + (i % 500) as u128;
                            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
                            let _ = book.add_limit_order_with_user(
                                id,
                                price,
                                10,
                                side,
                                TimeInForce::Gtc,
                                user,
                                None,
                            );
                        }
                        (book, user)
                    },
                    |(book, user)| {
                        let result = black_box(book.cancel_orders_by_user(user));
                        assert_eq!(result.cancelled_count(), count);
                    },
                );
            },
        );
    }

    group.finish();
}
