use criterion::Criterion;
use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce};
use std::hint::black_box;

/// Register benchmarks for mixed/realistic order book operations
pub fn register_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Mixed Operations");

    // Benchmark a realistic trading scenario with mixed operations
    group.bench_function("realistic_trading_scenario", |b| {
        b.iter(|| {
            let order_book: OrderBook = OrderBook::new("TEST-SYMBOL");

            // Phase 1: Add initial orders on both sides of the book
            for i in 0..50 {
                let bid_id = Id::new_uuid();
                let ask_id = Id::new_uuid();
                let _ = black_box(order_book.add_limit_order(
                    bid_id,
                    990 + i % 10,
                    10,
                    Side::Buy,
                    TimeInForce::Gtc,
                    None,
                ));
                let _ = black_box(order_book.add_limit_order(
                    ask_id,
                    1010 + i % 10,
                    10,
                    Side::Sell,
                    TimeInForce::Gtc,
                    None,
                ));
            }

            // Phase 2: Add some iceberg orders
            for _i in 0..10 {
                let bid_id = Id::new_uuid();
                let ask_id = Id::new_uuid();
                let _ = black_box(order_book.add_iceberg_order(
                    bid_id,
                    985,
                    5,
                    15,
                    Side::Buy,
                    TimeInForce::Gtc,
                    None,
                ));
                let _ = black_box(order_book.add_iceberg_order(
                    ask_id,
                    1015,
                    5,
                    15,
                    Side::Sell,
                    TimeInForce::Gtc,
                    None,
                ));
            }

            // Phase 3: Execute some market orders
            for i in 0..5 {
                let market_id = Id::new_uuid();
                let _ = black_box(order_book.submit_market_order(
                    market_id,
                    50,
                    if i % 2 == 0 { Side::Buy } else { Side::Sell },
                ));
            }

            // Phase 4: Cancel some orders
            let all_orders = order_book.get_all_orders();
            for (i, order) in all_orders.iter().enumerate() {
                if i % 5 == 0 {
                    let _ = black_box(order_book.cancel_order(order.id()));
                }
            }

            // Phase 5: Create a snapshot
            black_box(order_book.create_snapshot(5));
        })
    });

    // Benchmark high-frequency trading scenario
    group.bench_function("high_frequency_scenario", |b| {
        b.iter(|| {
            let order_book: OrderBook = OrderBook::new("TEST-SYMBOL");

            // Set up initial orderbook
            for i in 0..200 {
                let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
                let price = if side == Side::Buy {
                    1000 - (i / 2)
                } else {
                    1001 + (i / 2)
                };

                let id = Id::new_uuid();
                let _ = black_box(order_book.add_limit_order(
                    id,
                    price,
                    5,
                    side,
                    TimeInForce::Gtc,
                    None,
                ));
            }

            // Execute many small orders and modifications
            for i in 0..100 {
                let market_id = Id::new_uuid();
                let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };

                // Submit small market order
                let _ = black_box(order_book.submit_market_order(market_id, 2, side));

                // Add new limit order
                let limit_id = Id::new_uuid();
                let price = if side == Side::Buy {
                    999 - (i % 10)
                } else {
                    1001 + (i % 10)
                };
                let _ = black_box(order_book.add_limit_order(
                    limit_id,
                    price,
                    5,
                    side.opposite(),
                    TimeInForce::Gtc,
                    None,
                ));
            }
        })
    });

    group.finish();
}
