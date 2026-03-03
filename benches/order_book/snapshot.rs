use criterion::{BenchmarkId, Criterion};
use orderbook_rs::OrderBook;
use orderbook_rs::orderbook::snapshot::MetricFlags;
use pricelevel::{Id, Side, TimeInForce};
use std::hint::black_box;

/// Populate a book with `n` orders (half bids, half asks across price levels).
fn make_populated_book(n: usize) -> OrderBook<()> {
    let book = OrderBook::new("BENCH");
    for i in 0..n {
        let id = Id::new_uuid();
        let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
        let price = if side == Side::Buy {
            1000_u128.saturating_sub((i % 100) as u128)
        } else {
            1100_u128.saturating_add((i % 100) as u128)
        };
        let _ = book.add_limit_order(id, price, 10, side, TimeInForce::Gtc, None);
    }
    book
}

/// Register snapshot and enriched-snapshot benchmarks.
pub fn register_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Snapshot");

    // ─── create_snapshot ────────────────────────────────────────────
    for &order_count in &[100, 1_000, 10_000] {
        group.bench_with_input(
            BenchmarkId::new("create_snapshot", order_count),
            &order_count,
            |b, &count| {
                let book = make_populated_book(count);
                b.iter(|| black_box(book.create_snapshot(usize::MAX)));
            },
        );
    }

    // ─── restore_from_snapshot ──────────────────────────────────────
    for &order_count in &[100, 1_000, 10_000] {
        group.bench_with_input(
            BenchmarkId::new("restore_from_snapshot", order_count),
            &order_count,
            |b, &count| {
                let book = make_populated_book(count);
                let snap = book.create_snapshot(usize::MAX);
                b.iter_with_setup(
                    || snap.clone(),
                    |snapshot| {
                        let restored = OrderBook::<()>::new("BENCH");
                        let _ = black_box(restored.restore_from_snapshot(snapshot));
                    },
                );
            },
        );
    }

    // ─── enriched_snapshot_with_metrics (ALL) ───────────────────────
    for &order_count in &[100, 1_000, 10_000] {
        group.bench_with_input(
            BenchmarkId::new("enriched_snapshot_all", order_count),
            &order_count,
            |b, &count| {
                let book = make_populated_book(count);
                b.iter(|| {
                    black_box(book.enriched_snapshot_with_metrics(usize::MAX, MetricFlags::ALL))
                });
            },
        );
    }

    // ─── enriched_snapshot_with_metrics (MID_PRICE only) ────────────
    for &order_count in &[100, 1_000, 10_000] {
        group.bench_with_input(
            BenchmarkId::new("enriched_snapshot_mid_price", order_count),
            &order_count,
            |b, &count| {
                let book = make_populated_book(count);
                b.iter(|| {
                    black_box(
                        book.enriched_snapshot_with_metrics(usize::MAX, MetricFlags::MID_PRICE),
                    )
                });
            },
        );
    }

    // ─── snapshot JSON round-trip ───────────────────────────────────
    for &order_count in &[100, 1_000] {
        group.bench_with_input(
            BenchmarkId::new("snapshot_json_roundtrip", order_count),
            &order_count,
            |b, &count| {
                let book = make_populated_book(count);
                let snap = book.create_snapshot(usize::MAX);
                b.iter(|| {
                    let json = serde_json::to_vec(black_box(&snap))
                        .expect("json serialization must succeed");
                    let _: orderbook_rs::OrderBookSnapshot =
                        serde_json::from_slice(&json).expect("json deserialization must succeed");
                });
            },
        );
    }

    group.finish();
}
