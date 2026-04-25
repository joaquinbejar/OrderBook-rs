// examples/src/bin/prometheus_export.rs
//
// Operator demo of the optional `metrics` feature (issue #60).
//
// Builds an OrderBook, runs a small mix of accepted and rejected
// flow, then dumps the recorded counters / gauges in Prometheus
// text format via `metrics-exporter-prometheus`.
//
// Run with:
//
//     cd examples
//     cargo run --features metrics --bin prometheus_export
//
// Expected on stdout: a Prometheus exposition payload containing
//   * orderbook_rejects_total{reason="..."}
//   * orderbook_depth_levels_bid / orderbook_depth_levels_ask
//   * orderbook_trades_total

use metrics_exporter_prometheus::PrometheusBuilder;
use orderbook_rs::{OrderBook, OrderBookError};
use pricelevel::{Hash32, Id, Side, TimeInForce, setup_logger};
use tracing::{info, warn};

fn main() {
    let _ = setup_logger();
    info!("Prometheus export demo");

    // Install the Prometheus recorder. `install_recorder` returns a
    // handle whose `render()` method emits the current snapshot in
    // Prometheus text exposition format. In production you'd serve
    // that string over HTTP at /metrics.
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("install Prometheus recorder");

    let book = OrderBook::<()>::new("BTC/USD");

    // 1. Seed both sides with limit orders.
    seed_resting_book(&book);

    // 2. Cross a couple of trades to bump `orderbook_trades_total`.
    cross_some_trades(&book);

    // 3. Trigger a few rejects to populate `orderbook_rejects_total`.
    trigger_rejects(&book);

    // 4. Render the Prometheus exposition payload.
    let scrape = handle.render();
    info!("--- Prometheus exposition (current snapshot) ---");
    println!("{scrape}");
    info!("--- end of exposition ---");
}

fn seed_resting_book(book: &OrderBook<()>) {
    let user = Hash32::zero();

    let resting: [(u128, u64, Side); 6] = [
        (100, 5, Side::Buy),
        (99, 8, Side::Buy),
        (98, 3, Side::Buy),
        (101, 5, Side::Sell),
        (102, 6, Side::Sell),
        (103, 4, Side::Sell),
    ];

    for (price, qty, side) in resting {
        if let Err(err) = book.add_limit_order_with_user(
            Id::new_uuid(),
            price,
            qty,
            side,
            TimeInForce::Gtc,
            user,
            None,
        ) {
            warn!("seed add failed: {err}");
        }
    }
}

fn cross_some_trades(book: &OrderBook<()>) {
    // Aggressive buys against the resting asks.
    for (limit, qty) in [(102u128, 4u64), (103, 3)] {
        match book.add_limit_order(
            Id::new_uuid(),
            limit,
            qty,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        ) {
            Ok(_) => info!("aggressive buy filled at limit {limit} qty {qty}"),
            Err(err) => warn!("aggressive buy failed: {err}"),
        }
    }
}

fn trigger_rejects(book: &OrderBook<()>) {
    // Engage the kill switch to force a reject from the canonical
    // taxonomy. Releases immediately so the book still serves the
    // last metric render correctly.
    book.engage_kill_switch();
    let result = book.add_limit_order(Id::new_uuid(), 100, 1, Side::Buy, TimeInForce::Gtc, None);
    match result {
        Err(OrderBookError::KillSwitchActive) => {
            info!("expected KillSwitchActive reject recorded as a metric")
        }
        other => warn!("unexpected reject result: {other:?}"),
    }
    book.release_kill_switch();
}
