use criterion::{BatchSize, BenchmarkId, Criterion};
use orderbook_rs::OrderBook;
use orderbook_rs::orderbook::snapshot::MetricFlags;
use pricelevel::{Id, Side, TimeInForce};
use std::hint::black_box;
use uuid::Uuid;

/// Populate a book with `n` orders (half bids, half asks across price levels).
fn make_populated_book(n: usize) -> OrderBook<()> {
    let book = OrderBook::new("BENCH");
    for i in 0..n {
        let id = Id::from_uuid(Uuid::new_v4());
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
///
/// # Methodology (issue #258)
///
/// `book` is already built outside every `b.iter*` closure below (good —
/// the book itself was never part of the timed setup). What was missing
/// is that the plain `b.iter` timing loop also times the `Drop` of
/// whatever the closure returns (`elapsed = iters * (routine +
/// mem::drop(O))`, per `Bencher::iter`'s own doc). For `create_snapshot`
/// / `enriched_snapshot_with_metrics` at 10 000 orders the returned
/// snapshot is not a trivial value, so its drop was inflating the
/// reported cost of the *creation* these benches claim to measure.
/// `iter_with_large_drop` defers that drop until after the measurement
/// window closes. `restore_from_snapshot` had the more serious version
/// of the same bug: it built a brand-new, freshly-restored `OrderBook`
/// *inside* `routine` every sample and let it drop there too — so each
/// sample's cost included constructing and tearing down a whole
/// restored book, not just the restore call. `restore_from_snapshot`
/// takes `&self` and fully replaces every level on each call, so the
/// fix is to build the target book once, outside the loop, and restore
/// into it repeatedly.
pub fn register_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("OrderBook - Snapshot");

    // ─── create_snapshot ────────────────────────────────────────────
    for &order_count in &[100, 1_000, 10_000] {
        group.bench_with_input(
            BenchmarkId::new("create_snapshot", order_count),
            &order_count,
            |b, &count| {
                let book = make_populated_book(count);
                b.iter_with_large_drop(|| {
                    black_box(book.create_snapshot(usize::MAX).expect("snapshot"))
                });
            },
        );
    }

    // ─── create_snapshot, bounded depth (#313) ──────────────────────
    // Top-of-book view on a deep book: the walk stops after `depth`
    // non-empty levels per side instead of collecting every key.
    group.bench_function("create_snapshot_depth10", |b| {
        let book = make_populated_book(10_000);
        b.iter_with_large_drop(|| black_box(book.create_snapshot(10).expect("snapshot")));
    });

    // ─── restore_from_snapshot ──────────────────────────────────────
    for &order_count in &[100, 1_000, 10_000] {
        group.bench_with_input(
            BenchmarkId::new("restore_from_snapshot", order_count),
            &order_count,
            |b, &count| {
                let book = make_populated_book(count);
                let snap = book.create_snapshot(usize::MAX).expect("snapshot");
                // Restore into a single, reused target book: `restore_from_snapshot`
                // replaces every level on `&self`, so no per-sample book
                // construction/drop is needed — only `snap.clone()` (the
                // input the call consumes) is unmeasured setup.
                let restored = OrderBook::<()>::new("BENCH");
                b.iter_batched(
                    || snap.clone(),
                    |snapshot| {
                        restored
                            .restore_from_snapshot(snapshot)
                            .expect("restore_from_snapshot must succeed in benchmark");
                    },
                    BatchSize::SmallInput,
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
                b.iter_with_large_drop(|| {
                    black_box(
                        book.enriched_snapshot_with_metrics(usize::MAX, MetricFlags::ALL)
                            .expect("enriched snapshot"),
                    )
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
                b.iter_with_large_drop(|| {
                    black_box(
                        book.enriched_snapshot_with_metrics(usize::MAX, MetricFlags::MID_PRICE)
                            .expect("enriched snapshot"),
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
                let snap = book.create_snapshot(usize::MAX).expect("snapshot");
                b.iter_with_large_drop(|| {
                    let json = serde_json::to_vec(black_box(&snap))
                        .expect("json serialization must succeed");
                    let restored: orderbook_rs::OrderBookSnapshot =
                        serde_json::from_slice(&json).expect("json deserialization must succeed");
                    (json, restored)
                });
            },
        );
    }

    group.finish();
}
