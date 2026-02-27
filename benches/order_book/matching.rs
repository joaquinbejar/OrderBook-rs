use criterion::{Criterion, criterion_group, criterion_main};
use orderbook_rs::orderbook::book::OrderBook;
use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};
use serde::{Deserialize, Serialize};
use std::hint::black_box;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OrderMetadata {
    pub client_id: Option<u64>,
    pub user_id: Option<u64>,
    pub exchange_id: Option<u8>,
    pub priority: u8,
}

/// Sets up a deep order book for benchmarking.
/// Populates the book with a significant number of orders on the ask side.
fn setup_deep_book() -> OrderBook<OrderMetadata> {
    let book = OrderBook::new("BENCH_SYMBOL");

    // Create 100 price levels, from 10001 to 10100
    for i in 0..100 {
        let price = 10001 + i;
        // Add 10 orders at each price level
        for _ in 0..10 {
            let order = OrderType::Standard {
                id: Id::new(),
                side: Side::Sell,
                price: Price::new(price),
                quantity: Quantity::new(10),
                user_id: Hash32::zero(),
                time_in_force: TimeInForce::Gtc,
                timestamp: TimestampMs::new(0),
                extra_fields: OrderMetadata {
                    client_id: Some(1),
                    user_id: Some(2),
                    exchange_id: Some(3),
                    priority: 4,
                },
            };
            book.add_order(order).unwrap();
        }
    }
    book
}

/// Benchmark for matching a large market order that consumes a significant portion of the book.
pub fn register_benchmarks(c: &mut Criterion) {
    let book = setup_deep_book();

    c.bench_function("match_order_deep_book", |b| {
        b.iter(|| {
            // The order to match. Its quantity (505) is chosen to match across
            // multiple price levels (50 levels + 5 from the 51st).
            let taker_order_id = Id::new();
            book.match_order(
                black_box(taker_order_id),
                black_box(Side::Buy),
                black_box(505),
                black_box(None), // Market order
            )
        })
    });
}

criterion_group!(benches, register_benchmarks);
criterion_main!(benches);
