//! Pluggable timestamp source for the matching core.
//!
//! The [`Clock`] trait abstracts the source of millisecond timestamps used
//! by the order book to stamp inbound orders, snapshots, and lifecycle
//! transitions. Two implementations are provided:
//!
//! - [`MonotonicClock`] — wraps [`crate::utils::current_time_millis`] for
//!   production deployments. Returns wall-clock milliseconds since the
//!   Unix epoch.
//! - [`StubClock`] — a deterministic counter-based clock for tests and
//!   byte-identical sequencer replay. Each call to `now_millis` advances
//!   an internal counter by a fixed `step` (default `1`).
//!
//! Both implementations are `Send + Sync` so an `Arc<dyn Clock>` can be
//! shared across threads. The trait is object-safe and is stored on
//! [`crate::orderbook::book::OrderBook`] as `Arc<dyn Clock>`.

use pricelevel::TimestampMs;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// A source of wall-clock or logical millisecond timestamps for the
/// matching core.
///
/// Implementations must be [`Send`] + [`Sync`] because an
/// `Arc<dyn Clock>` is held by the order book and potentially shared
/// across threads. The trait is object-safe: it is stored as
/// `Arc<dyn Clock>` on the book so different deployments (production,
/// replay, deterministic tests) can swap the implementation without
/// changing the generic parameterization of the order book.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Current millisecond timestamp.
    ///
    /// Semantics depend on the implementation:
    /// - production ([`MonotonicClock`]): wall-clock milliseconds since
    ///   the Unix epoch.
    /// - replay / test ([`StubClock`]): a monotonic logical counter,
    ///   not wall-clock.
    fn now_millis(&self) -> TimestampMs;
}

/// Production clock wrapping [`crate::utils::current_time_millis`].
///
/// Returns wall-clock milliseconds since the Unix epoch. This is the
/// default clock installed on every [`crate::orderbook::book::OrderBook`]
/// constructed via [`crate::orderbook::book::OrderBook::new`] and its
/// friends.
#[derive(Debug, Default, Clone, Copy)]
pub struct MonotonicClock;

impl Clock for MonotonicClock {
    #[inline]
    fn now_millis(&self) -> TimestampMs {
        TimestampMs::new(crate::utils::current_time_millis())
    }
}

/// Deterministic stub clock. Each call to [`Clock::now_millis`] advances
/// an internal counter by `step` (default `1` millisecond). Intended for
/// sequencer replay, proptests, and snapshot tests that require
/// byte-identical timestamps across runs.
#[derive(Debug)]
pub struct StubClock {
    counter: AtomicU64,
    step: u64,
}

impl StubClock {
    /// Create a new stub clock starting at `0` with a step of `1` ms.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
            step: 1,
        }
    }

    /// Create a new stub clock starting at `start` with a step of `1` ms.
    #[must_use]
    pub fn starting_at(start: u64) -> Self {
        Self {
            counter: AtomicU64::new(start),
            step: 1,
        }
    }

    /// Create a new stub clock starting at `start` with a custom step.
    ///
    /// Each call to [`Clock::now_millis`] advances the counter by `step`.
    #[must_use]
    pub fn with_step(start: u64, step: u64) -> Self {
        Self {
            counter: AtomicU64::new(start),
            step,
        }
    }

    /// Current counter value without advancing. Test-only helper.
    #[must_use]
    pub fn peek(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }
}

impl Default for StubClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for StubClock {
    #[inline]
    fn now_millis(&self) -> TimestampMs {
        let v = self.counter.fetch_add(self.step, Ordering::Relaxed);
        TimestampMs::new(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_monotonic_clock_returns_nonzero_ms() {
        let clock = MonotonicClock;
        let ts = clock.now_millis();
        assert!(ts.as_u64() > 0, "wall-clock must be after the epoch");
    }

    #[test]
    fn test_stub_clock_starts_at_zero_and_advances_by_one() {
        let clock = StubClock::new();
        assert_eq!(clock.now_millis().as_u64(), 0);
        assert_eq!(clock.now_millis().as_u64(), 1);
        assert_eq!(clock.now_millis().as_u64(), 2);
    }

    #[test]
    fn test_stub_clock_with_step() {
        let clock = StubClock::with_step(100, 5);
        assert_eq!(clock.now_millis().as_u64(), 100);
        assert_eq!(clock.now_millis().as_u64(), 105);
        assert_eq!(clock.now_millis().as_u64(), 110);
    }

    #[test]
    fn test_stub_clock_peek_does_not_advance() {
        let clock = StubClock::starting_at(42);
        assert_eq!(clock.peek(), 42);
        assert_eq!(clock.peek(), 42);
        let first = clock.now_millis();
        assert_eq!(first.as_u64(), 42);
        assert_eq!(clock.peek(), 43);
    }

    #[test]
    fn test_stub_clock_concurrent_advance_is_monotonic() {
        // 4 threads x 1000 calls each, step = 1, start = 0.
        // Collect every returned value across every thread; the full set
        // must contain exactly 4000 unique values and the max must equal
        // 4000 * step - 1 + start = 3999.
        let start: u64 = 0;
        let step: u64 = 1;
        let threads = 4usize;
        let per_thread = 1000usize;
        let total = threads.checked_mul(per_thread).expect("overflow");

        let clock = Arc::new(StubClock::with_step(start, step));
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let c = Arc::clone(&clock);
            handles.push(thread::spawn(move || {
                let mut local = Vec::with_capacity(per_thread);
                for _ in 0..per_thread {
                    local.push(c.now_millis().as_u64());
                }
                local
            }));
        }

        let mut all: Vec<u64> = Vec::with_capacity(total);
        for h in handles {
            let part = h.join().expect("thread panicked");
            all.extend(part);
        }

        assert_eq!(all.len(), total);
        let set: HashSet<u64> = all.iter().copied().collect();
        assert_eq!(
            set.len(),
            total,
            "expected every observed tick to be unique"
        );

        let expected_max = start
            .checked_add(
                (total as u64)
                    .checked_mul(step)
                    .and_then(|v| v.checked_sub(1))
                    .expect("overflow"),
            )
            .expect("overflow");
        let observed_max = all.iter().copied().max().expect("non-empty");
        assert_eq!(observed_max, expected_max);
    }
}
