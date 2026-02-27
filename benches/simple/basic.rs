use criterion::Criterion;
use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce};

pub fn benchmark_data(c: &mut Criterion) {
    let mut group = c.benchmark_group("Basic OrderBook Operations");

    // Benchmark for creating a new order book
    group.bench_function("create_order_book", |b| {
        b.iter(|| {
            let _order_book: OrderBook = OrderBook::new("TEST-SYMBOL");
        })
    });

    // Benchmark for creating and adding a single order
    group.bench_function("add_single_order", |b| {
        b.iter(|| {
            let order_book: OrderBook = OrderBook::new("TEST-SYMBOL");
            let id = Id::new_uuid();
            let _ = order_book.add_limit_order(id, 1000, 10, Side::Buy, TimeInForce::Gtc, None);
        })
    });

    group.finish();
}
