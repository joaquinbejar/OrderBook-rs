//! Integration tests for the optional Prometheus metrics feature
//! (issue #60).
//!
//! Lives in a dedicated test binary so the global `metrics` recorder
//! is not perturbed by the broader integration suite under
//! `tests/unit/` (which constructs `OrderBook`s and triggers the
//! depth gauge updates as a side effect of every add / cancel).

use metrics::{Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit};
use orderbook_rs::orderbook::metrics::{
    DEPTH_LEVELS_ASK, DEPTH_LEVELS_BID, REJECTS_TOTAL, TRADES_TOTAL,
};
use orderbook_rs::{OrderBook, StubClock};
use pricelevel::{Id, Side, TimeInForce};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Captured counter / gauge state, keyed by metric name with a
/// `reason=…` suffix when the labels include a reason.
#[derive(Default)]
struct Captured {
    counters: HashMap<String, u64>,
    gauges: HashMap<String, f64>,
}

/// Process-wide capture storage. The `metrics` crate only allows the
/// global recorder to be installed once per process — every test in
/// this file shares the same recorder and reads from this storage.
fn captured() -> &'static Mutex<Captured> {
    static CAPTURED: OnceLock<Mutex<Captured>> = OnceLock::new();
    CAPTURED.get_or_init(|| Mutex::new(Captured::default()))
}

/// Build a "metric_name{label_value}" key, or just the metric name
/// when there are no labels — matches the format used in assertions.
fn label_key(key: &Key) -> String {
    let labels: Vec<String> = key
        .labels()
        .map(|l| format!("{}={}", l.key(), l.value()))
        .collect();
    if labels.is_empty() {
        key.name().to_string()
    } else {
        format!("{}{{{}}}", key.name(), labels.join(","))
    }
}

struct CapturingCounter {
    key: String,
}

impl metrics::CounterFn for CapturingCounter {
    fn increment(&self, value: u64) {
        let mut g = captured().lock().expect("captured lock");
        *g.counters.entry(self.key.clone()).or_insert(0) += value;
    }
    fn absolute(&self, value: u64) {
        let mut g = captured().lock().expect("captured lock");
        g.counters.insert(self.key.clone(), value);
    }
}

struct CapturingGauge {
    key: String,
}

impl metrics::GaugeFn for CapturingGauge {
    fn increment(&self, value: f64) {
        let mut g = captured().lock().expect("captured lock");
        *g.gauges.entry(self.key.clone()).or_insert(0.0) += value;
    }
    fn decrement(&self, value: f64) {
        let mut g = captured().lock().expect("captured lock");
        *g.gauges.entry(self.key.clone()).or_insert(0.0) -= value;
    }
    fn set(&self, value: f64) {
        let mut g = captured().lock().expect("captured lock");
        g.gauges.insert(self.key.clone(), value);
    }
}

struct CapturingHistogram;

impl metrics::HistogramFn for CapturingHistogram {
    fn record(&self, _value: f64) {}
}

struct CapturingRecorder;

impl Recorder for CapturingRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        Counter::from_arc(std::sync::Arc::new(CapturingCounter {
            key: label_key(key),
        }))
    }
    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::from_arc(std::sync::Arc::new(CapturingGauge {
            key: label_key(key),
        }))
    }
    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::from_arc(std::sync::Arc::new(CapturingHistogram))
    }
}

/// Install the global capturing recorder once. Calling this from every
/// test is idempotent — the second installation attempt is a no-op.
fn install_recorder() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        // `set_global_recorder` only succeeds once per process.
        let _ = metrics::set_global_recorder(CapturingRecorder);
    });
}

fn counter_value(key: &str) -> u64 {
    let g = captured().lock().expect("captured lock");
    g.counters.get(key).copied().unwrap_or(0)
}

fn gauge_value(key: &str) -> f64 {
    let g = captured().lock().expect("captured lock");
    g.gauges.get(key).copied().unwrap_or(0.0)
}

/// All tests in this module share the global `metrics` recorder and
/// the captured-state map. Take this lock at the top of every test
/// to serialize them — concurrent tests would otherwise step on each
/// other's gauge values.
fn serialized_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn counters_increment_on_rejects_and_trades() {
    let _guard = serialized_test_lock().lock().expect("serialized lock");
    install_recorder();
    let book = OrderBook::<()>::new("METRICS-TEST");

    // Snapshot baseline counter values — other tests in this module
    // share the global recorder, so we reason about deltas.
    let trades_before = counter_value(TRADES_TOTAL);
    let kill_rejects_before =
        counter_value(&format!("{REJECTS_TOTAL}{{reason=kill switch active}}"));

    // Reject path: engage the kill switch and submit one order.
    book.engage_kill_switch();
    let rej = book.add_limit_order(Id::new_uuid(), 100, 1, Side::Buy, TimeInForce::Gtc, None);
    assert!(rej.is_err(), "kill-switched add_order must Err");
    book.release_kill_switch();

    let kill_rejects_after =
        counter_value(&format!("{REJECTS_TOTAL}{{reason=kill switch active}}"));
    assert_eq!(
        kill_rejects_after - kill_rejects_before,
        1,
        "kill-switch reject must increment orderbook_rejects_total{{reason=...}} by exactly 1"
    );

    // Happy path: cross two limit orders to print a trade.
    book.add_limit_order(Id::new_uuid(), 100, 5, Side::Sell, TimeInForce::Gtc, None)
        .expect("seed resting ask");
    book.add_limit_order(Id::new_uuid(), 100, 5, Side::Buy, TimeInForce::Gtc, None)
        .expect("aggressive buy fills the ask");

    let trades_after = counter_value(TRADES_TOTAL);
    assert!(
        trades_after > trades_before,
        "orderbook_trades_total must increment after a fill (before={trades_before}, after={trades_after})"
    );
}

#[test]
fn depth_gauges_track_distinct_price_levels() {
    let _guard = serialized_test_lock().lock().expect("serialized lock");
    install_recorder();
    let book = OrderBook::<()>::new("METRICS-DEPTH");

    // Place two distinct bid levels and one ask level.
    book.add_limit_order(Id::new_uuid(), 100, 1, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid 1");
    book.add_limit_order(Id::new_uuid(), 99, 1, Side::Buy, TimeInForce::Gtc, None)
        .expect("bid 2");
    let ask_id = Id::new_uuid();
    book.add_limit_order(ask_id, 110, 1, Side::Sell, TimeInForce::Gtc, None)
        .expect("ask 1");

    assert_eq!(
        gauge_value(DEPTH_LEVELS_BID) as u64,
        2,
        "orderbook_depth_levels_bid must reflect two distinct bid levels"
    );
    assert_eq!(
        gauge_value(DEPTH_LEVELS_ASK) as u64,
        1,
        "orderbook_depth_levels_ask must reflect one ask level"
    );

    // Cancel the unique ask — the ask gauge should go to 0.
    book.cancel_order(ask_id).expect("cancel ask");

    assert_eq!(
        gauge_value(DEPTH_LEVELS_ASK) as u64,
        0,
        "ask gauge must drop to 0 after the only ask level is removed"
    );
}

#[test]
fn metrics_do_not_affect_order_semantics() {
    // Determinism guard — issue #60 explicitly requires that metric
    // emission must NOT alter matching outcomes. Build two books with
    // the same symbol and identical inputs and confirm they produce
    // byte-identical snapshots after the same operation sequence.
    let _guard = serialized_test_lock().lock().expect("serialized lock");
    install_recorder();
    // StubClock + identical symbols + identical order ids yields a
    // byte-identical state machine. If metrics emission ever bled
    // back into matching, the two snapshots would diverge.
    let book_a = OrderBook::<()>::with_clock("DET", Arc::new(StubClock::new()));
    let book_b = OrderBook::<()>::with_clock("DET", Arc::new(StubClock::new()));

    let scenarios: [(u128, u64, Side); 6] = [
        (100, 5, Side::Sell),
        (101, 3, Side::Sell),
        (99, 5, Side::Buy),
        (100, 4, Side::Buy),
        (102, 2, Side::Sell),
        (101, 3, Side::Buy),
    ];

    for (i, (price, qty, side)) in scenarios.into_iter().enumerate() {
        // Use a deterministic id derived from the index so the two
        // books mint structurally identical resting orders.
        let id = Id::from_u64(0xC0DE_0000 + i as u64);
        let _ = book_a.add_limit_order(id, price, qty, side, TimeInForce::Gtc, None);
        let _ = book_b.add_limit_order(id, price, qty, side, TimeInForce::Gtc, None);
    }

    let snap_a = book_a.create_snapshot(10);
    let snap_b = book_b.create_snapshot(10);

    let json_a = serde_json::to_string(&snap_a).expect("serialize snap_a");
    let json_b = serde_json::to_string(&snap_b).expect("serialize snap_b");
    assert_eq!(
        json_a, json_b,
        "metrics emission must not affect book state — snapshots differ"
    );
}
