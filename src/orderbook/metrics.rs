//! Operational Prometheus-style metrics for the order book core.
//!
//! Issue #60 — feature-gated, additive observability hooks. When the
//! `metrics` feature is enabled the helpers in this module forward to
//! the `metrics` crate's global recorder; when the feature is off every
//! helper compiles down to a no-op so that call-sites in the matching
//! hot path stay unconditional and allocation-free.
//!
//! # Metrics surface
//!
//! - `orderbook_rejects_total{reason="…"}` — counter, incremented on
//!   every rejection that flows through `record_reject`. The label
//!   value is the [`RejectReason`] [`Display`] string (stable across
//!   `0.7.x`).
//! - `orderbook_depth_levels_bid` / `orderbook_depth_levels_ask` —
//!   gauges, updated on every book change to reflect the current count
//!   of distinct price levels on each side.
//! - `orderbook_trades_total` — counter, incremented exactly once per
//!   emitted trade transaction (a `MatchResult` may contain several).
//!
//! # Determinism
//!
//! Metrics emission is **out-of-band**: it does not influence matching,
//! does not allocate on the happy path, and does not cross the
//! determinism boundary. `restore_from_snapshot_package` deliberately
//! does **not** rehydrate metric counters — they are operational only
//! and live for the process lifetime.
//!
//! [`RejectReason`]: crate::orderbook::reject_reason::RejectReason
//! [`Display`]: std::fmt::Display

use crate::orderbook::reject_reason::RejectReason;

/// Counter name: total order rejections, labelled by reject reason.
pub const REJECTS_TOTAL: &str = "orderbook_rejects_total";

/// Gauge name: current count of distinct bid price levels.
pub const DEPTH_LEVELS_BID: &str = "orderbook_depth_levels_bid";

/// Gauge name: current count of distinct ask price levels.
pub const DEPTH_LEVELS_ASK: &str = "orderbook_depth_levels_ask";

/// Counter name: monotonic count of every emitted trade transaction.
pub const TRADES_TOTAL: &str = "orderbook_trades_total";

/// Record an order rejection.
///
/// Increments `orderbook_rejects_total` by 1 with the
/// `reason="<RejectReason::Display>"` label. Compiles to a no-op when
/// the `metrics` feature is disabled.
#[inline]
#[cfg(feature = "metrics")]
pub fn record_reject(reason: RejectReason) {
    let label = reason.to_string();
    metrics::counter!(REJECTS_TOTAL, "reason" => label).increment(1);
}

/// No-op when the `metrics` feature is disabled.
#[inline]
#[cfg(not(feature = "metrics"))]
pub fn record_reject(_reason: RejectReason) {}

/// Update the bid / ask depth gauges to the supplied counts.
///
/// Called from book-change emission paths. Compiles to a no-op when
/// the `metrics` feature is disabled.
#[inline]
#[cfg(feature = "metrics")]
pub fn record_depth(bid_levels: u64, ask_levels: u64) {
    // `gauge!` accepts an `f64`; the input is a level count that
    // comfortably fits in `f64` precision for any realistic book.
    metrics::gauge!(DEPTH_LEVELS_BID).set(bid_levels as f64);
    metrics::gauge!(DEPTH_LEVELS_ASK).set(ask_levels as f64);
}

/// No-op when the `metrics` feature is disabled.
#[inline]
#[cfg(not(feature = "metrics"))]
pub fn record_depth(_bid_levels: u64, _ask_levels: u64) {}

/// Record `n` newly emitted trade transactions.
///
/// Called once per `TradeListener` callback with the number of
/// transactions in the underlying `MatchResult`. Compiles to a no-op
/// when the `metrics` feature is disabled.
#[inline]
#[cfg(feature = "metrics")]
pub fn record_trades(n: u64) {
    if n == 0 {
        return;
    }
    metrics::counter!(TRADES_TOTAL).increment(n);
}

/// No-op when the `metrics` feature is disabled.
#[inline]
#[cfg(not(feature = "metrics"))]
pub fn record_trades(_n: u64) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// All four call-sites must compile and run without panicking
    /// regardless of feature state. The actual counter behaviour is
    /// covered by `tests/metrics/` (feature-gated).
    #[test]
    fn helpers_are_callable_unconditionally() {
        record_reject(RejectReason::KillSwitchActive);
        record_reject(RejectReason::Other(7777));
        record_depth(0, 0);
        record_depth(3, 5);
        record_trades(0);
        record_trades(4);
    }
}
