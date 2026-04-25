//! Order state machine for explicit lifecycle tracking.
//!
//! This module provides [`OrderStatus`], [`CancelReason`],
//! [`OrderStateTracker`], and [`OrderStateListener`] to track the full
//! lifecycle of every order from submission to terminal state.
//!
//! The tracker is an **optional** component on [`OrderBook`] — when not
//! configured, there is zero overhead on the matching hot path.
//!
//! [`OrderBook`]: super::OrderBook
//!
//! # State Transitions
//!
//! ```text
//! add_order (success, no match)    → Open
//! add_order (partial match)        → PartiallyFilled / Filled
//! add_order (rejected)             → Rejected
//! matching (resting order fills)   → PartiallyFilled → Filled
//! cancel_order                     → Cancelled { UserRequested }
//! mass_cancel_*                    → Cancelled { MassCancel* }
//! STP                              → Cancelled { SelfTradePrevention }
//! IOC/FOK insufficient liquidity   → Cancelled { InsufficientLiquidity }
//! ```

use super::clock::{Clock, MonotonicClock};
use super::reject_reason::RejectReason;
use dashmap::DashMap;
use pricelevel::Id;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Reason for order cancellation.
///
/// Each variant identifies the specific mechanism that triggered the
/// cancellation, enabling upstream services to provide detailed
/// notifications to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum CancelReason {
    /// Cancelled by explicit user request via `cancel_order`.
    UserRequested,
    /// Cancelled by Self-Trade Prevention logic.
    SelfTradePrevention,
    /// Cancelled because the order's time-in-force expired.
    TimeInForceExpired,
    /// Cancelled by `cancel_all_orders`.
    MassCancelAll,
    /// Cancelled by `cancel_orders_by_side`.
    MassCancelBySide,
    /// Cancelled by `cancel_orders_by_user`.
    MassCancelByUser,
    /// Cancelled by `cancel_orders_by_price_range`.
    MassCancelByPriceRange,
    /// IOC or FOK order could not be fully filled.
    InsufficientLiquidity,
}

impl std::fmt::Display for CancelReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserRequested => write!(f, "user requested"),
            Self::SelfTradePrevention => write!(f, "self-trade prevention"),
            Self::TimeInForceExpired => write!(f, "time-in-force expired"),
            Self::MassCancelAll => write!(f, "mass cancel all"),
            Self::MassCancelBySide => write!(f, "mass cancel by side"),
            Self::MassCancelByUser => write!(f, "mass cancel by user"),
            Self::MassCancelByPriceRange => write!(f, "mass cancel by price range"),
            Self::InsufficientLiquidity => write!(f, "insufficient liquidity"),
        }
    }
}

/// Explicit order status for lifecycle tracking.
///
/// Every order transitions through a subset of these states. Terminal
/// states (`Filled`, `Cancelled`, `Rejected`) are retained by the
/// [`OrderStateTracker`] up to a configurable capacity for post-trade
/// queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    /// Order accepted and resting in the book, no fills yet.
    Open,

    /// Order partially filled, remainder still resting in the book.
    PartiallyFilled {
        /// Total quantity originally submitted.
        original_quantity: u64,
        /// Quantity filled so far.
        filled_quantity: u64,
    },

    /// Order fully filled and removed from the book.
    Filled {
        /// Total quantity filled.
        filled_quantity: u64,
    },

    /// Order cancelled (by user, STP, mass cancel, or expiry).
    Cancelled {
        /// Quantity filled before cancellation (0 if none).
        filled_quantity: u64,
        /// Reason for cancellation.
        reason: CancelReason,
    },

    /// Order rejected during validation (never entered the book).
    Rejected {
        /// Closed wire-side reject code. See [`RejectReason`].
        reason: RejectReason,
    },
}

impl OrderStatus {
    /// Returns `true` if this is a terminal state (no further transitions).
    #[must_use]
    #[inline]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OrderStatus::Filled { .. }
                | OrderStatus::Cancelled { .. }
                | OrderStatus::Rejected { .. }
        )
    }

    /// Returns `true` if the order is still active in the book.
    #[must_use]
    #[inline]
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            OrderStatus::Open | OrderStatus::PartiallyFilled { .. }
        )
    }

    /// Returns the filled quantity, or 0 for `Open` and `Rejected`.
    #[must_use]
    #[inline]
    pub fn filled_quantity(&self) -> u64 {
        match self {
            OrderStatus::Open => 0,
            OrderStatus::PartiallyFilled {
                filled_quantity, ..
            } => *filled_quantity,
            OrderStatus::Filled { filled_quantity } => *filled_quantity,
            OrderStatus::Cancelled {
                filled_quantity, ..
            } => *filled_quantity,
            OrderStatus::Rejected { .. } => 0,
        }
    }
}

impl std::fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderStatus::Open => write!(f, "Open"),
            OrderStatus::PartiallyFilled {
                original_quantity,
                filled_quantity,
            } => write!(f, "PartiallyFilled({filled_quantity}/{original_quantity})"),
            OrderStatus::Filled { filled_quantity } => {
                write!(f, "Filled({filled_quantity})")
            }
            OrderStatus::Cancelled {
                filled_quantity,
                reason,
            } => write!(f, "Cancelled({reason}, filled={filled_quantity})"),
            OrderStatus::Rejected { reason } => write!(f, "Rejected({reason})"),
        }
    }
}

/// Callback invoked on every order state transition.
///
/// The listener receives the order ID, the previous status, and the new
/// status. Listeners are called synchronously from the thread performing
/// the book operation and must not block.
///
/// # Arguments
///
/// * `order_id` — the order whose status changed
/// * `old_status` — the previous status (or the new status if this is the
///   first transition, i.e., `Open` or `Rejected`)
/// * `new_status` — the status after the transition
pub type OrderStateListener = Arc<dyn Fn(Id, &OrderStatus, &OrderStatus) + Send + Sync>;

/// Default number of terminal-state entries to retain before eviction.
const DEFAULT_RETENTION_CAPACITY: usize = 10_000;

/// Thread-safe tracker for order lifecycle states.
///
/// Stores the current [`OrderStatus`] for every order that has been
/// submitted to the book. Terminal states (`Filled`, `Cancelled`,
/// `Rejected`) are retained up to a configurable capacity (default 10,000);
/// when the limit is exceeded, the oldest terminal entries are evicted (FIFO).
///
/// # Thread Safety
///
/// Uses [`DashMap`] for lock-free concurrent reads and writes. The
/// terminal-state eviction queue uses a [`Mutex`]-protected
/// [`VecDeque`]; this is acceptable because eviction only happens on
/// terminal transitions (not on the matching hot path).
///
/// # Example
///
/// ```
/// use orderbook_rs::orderbook::order_state::OrderStateTracker;
///
/// let tracker = OrderStateTracker::new();
/// assert_eq!(tracker.len(), 0);
/// ```
pub struct OrderStateTracker {
    /// Current status of each tracked order.
    states: DashMap<Id, OrderStatus>,
    /// Timestamped transition history per order: `(timestamp_ms, status)`.
    ///
    /// Timestamps are the millisecond values returned by the installed
    /// [`Clock`]. History grows linearly with transitions for each order
    /// (e.g. many partial fills). Entries are evicted together with
    /// their state both by capacity-based eviction in
    /// [`enqueue_terminal`](Self::enqueue_terminal) and by
    /// [`purge_terminal_older_than`](Self::purge_terminal_older_than).
    history: DashMap<Id, Vec<(u64, OrderStatus)>>,
    /// FIFO queue of terminal-state order IDs for eviction.
    terminal_queue: Mutex<VecDeque<Id>>,
    /// Maximum number of terminal-state entries to retain.
    retention_capacity: usize,
    /// Optional listener invoked on every state transition.
    listener: Option<OrderStateListener>,
    /// Pluggable source of millisecond timestamps used when recording
    /// transition history and when computing cutoffs for
    /// [`purge_terminal_older_than`](Self::purge_terminal_older_than).
    /// Defaults to [`MonotonicClock`]; tests and sequencer replay can
    /// inject a [`super::clock::StubClock`] via [`Self::with_clock`] or
    /// [`Self::with_capacity_and_clock`].
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for OrderStateTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrderStateTracker")
            .field("tracked_orders", &self.states.len())
            .field("retention_capacity", &self.retention_capacity)
            .field("has_listener", &self.listener.is_some())
            .finish()
    }
}

impl Default for OrderStateTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderStateTracker {
    /// Create a new tracker with default retention capacity (10,000).
    ///
    /// The tracker uses a [`MonotonicClock`] to stamp transition history.
    /// Use [`Self::with_clock`] to inject a different [`Clock`]
    /// implementation (e.g. a
    /// [`super::clock::StubClock`] for deterministic tests).
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(MonotonicClock) as Arc<dyn Clock>)
    }

    /// Create a new tracker with default retention capacity and a
    /// caller-provided [`Clock`] implementation.
    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            states: DashMap::new(),
            history: DashMap::new(),
            terminal_queue: Mutex::new(VecDeque::new()),
            retention_capacity: DEFAULT_RETENTION_CAPACITY,
            listener: None,
            clock,
        }
    }

    /// Create a new tracker with a custom retention capacity.
    ///
    /// # Arguments
    ///
    /// * `retention_capacity` — maximum number of terminal-state entries
    ///   to retain. When exceeded, the oldest entries are evicted.
    #[must_use]
    pub fn with_capacity(retention_capacity: usize) -> Self {
        Self::with_capacity_and_clock(
            retention_capacity,
            Arc::new(MonotonicClock) as Arc<dyn Clock>,
        )
    }

    /// Create a new tracker with a custom retention capacity and a
    /// caller-provided [`Clock`] implementation.
    #[must_use]
    pub fn with_capacity_and_clock(retention_capacity: usize, clock: Arc<dyn Clock>) -> Self {
        Self {
            states: DashMap::new(),
            history: DashMap::new(),
            terminal_queue: Mutex::new(VecDeque::new()),
            retention_capacity,
            listener: None,
            clock,
        }
    }

    /// Set the listener that will be invoked on every state transition.
    ///
    /// Only one listener is supported. Setting a new listener replaces
    /// the previous one.
    pub fn set_listener(&mut self, listener: OrderStateListener) {
        self.listener = Some(listener);
    }

    /// Returns the current status of an order, or `None` if unknown.
    #[must_use]
    pub fn get(&self, order_id: Id) -> Option<OrderStatus> {
        self.states
            .get(&order_id)
            .map(|entry| entry.value().clone())
    }

    /// Returns the number of tracked orders (active + retained terminal).
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Returns `true` if no orders are being tracked.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Record a new status for an order.
    ///
    /// If the order already has a status, the listener (if any) is called
    /// with both old and new. If this is the first status for the order,
    /// the listener receives the new status as both old and new.
    ///
    /// Terminal states trigger eviction of the oldest terminal entries
    /// when `retention_capacity` is exceeded.
    pub fn transition(&self, order_id: Id, new_status: OrderStatus) {
        let old_status = self
            .states
            .get(&order_id)
            .map(|entry| entry.value().clone());

        self.states.insert(order_id, new_status.clone());

        // Record timestamped history. The timestamp is in milliseconds,
        // sourced from the installed [`Clock`] (wall-clock in production,
        // logical counter under replay / tests).
        let ts = self.clock.now_millis().as_u64();
        self.history
            .entry(order_id)
            .or_default()
            .push((ts, new_status.clone()));

        // Notify listener
        if let Some(ref listener) = self.listener {
            let old = old_status.as_ref().unwrap_or(&new_status);
            listener(order_id, old, &new_status);
        }

        // Track terminal states for eviction
        if new_status.is_terminal() {
            self.enqueue_terminal(order_id);
        }
    }

    /// Add a terminal order ID to the eviction queue and evict if needed.
    fn enqueue_terminal(&self, order_id: Id) {
        if let Ok(mut queue) = self.terminal_queue.lock() {
            queue.push_back(order_id);
            while queue.len() > self.retention_capacity {
                if let Some(evicted_id) = queue.pop_front() {
                    // Only evict if still in terminal state (not overwritten)
                    if let Some(entry) = self.states.get(&evicted_id)
                        && entry.value().is_terminal()
                    {
                        drop(entry);
                        self.states.remove(&evicted_id);
                        self.history.remove(&evicted_id);
                    }
                }
            }
        }
    }

    /// Returns the full transition history for an order.
    ///
    /// Each entry is a `(timestamp_ms, OrderStatus)` pair in chronological
    /// order. Timestamps come from the installed [`Clock`]
    /// ([`MonotonicClock`] by default). Returns `None` if the order ID
    /// was never submitted.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::orderbook::order_state::{OrderStateTracker, OrderStatus};
    /// use pricelevel::Id;
    ///
    /// let tracker = OrderStateTracker::new();
    /// let id = Id::new_uuid();
    /// tracker.transition(id, OrderStatus::Open);
    /// let history = tracker.get_history(id);
    /// assert!(history.is_some());
    /// assert_eq!(history.as_ref().map(|h| h.len()), Some(1));
    /// ```
    #[must_use]
    pub fn get_history(&self, order_id: Id) -> Option<Vec<(u64, OrderStatus)>> {
        self.history
            .get(&order_id)
            .map(|entry| entry.value().clone())
    }

    /// Returns the number of orders currently in an active state
    /// (`Open` or `PartiallyFilled`).
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.states.iter().filter(|e| e.value().is_active()).count()
    }

    /// Returns the number of orders currently in a terminal state
    /// (`Filled`, `Cancelled`, or `Rejected`).
    #[must_use]
    pub fn terminal_count(&self) -> usize {
        self.states
            .iter()
            .filter(|e| e.value().is_terminal())
            .count()
    }

    /// Remove all terminal-state entries whose last transition is older
    /// than `older_than` ago.
    ///
    /// Active orders (`Open`, `PartiallyFilled`) are never purged.
    /// This is useful for bounded memory management in long-running
    /// processes.
    ///
    /// # Arguments
    ///
    /// * `older_than` — entries with a last-transition timestamp older
    ///   than `now - older_than` (milliseconds, per the installed
    ///   [`Clock`]) are removed.
    ///
    /// # Returns
    ///
    /// The number of entries purged.
    pub fn purge_terminal_older_than(&self, older_than: Duration) -> usize {
        let now_ms = self.clock.now_millis().as_u64();
        let cutoff =
            now_ms.saturating_sub(u64::try_from(older_than.as_millis()).unwrap_or(u64::MAX));

        let mut purged = 0usize;
        // Collect IDs to remove (avoid holding DashMap iterators during mutation)
        let to_remove: Vec<Id> = self
            .states
            .iter()
            .filter_map(|entry| {
                let id = *entry.key();
                let status = entry.value();
                if !status.is_terminal() {
                    return None;
                }
                // Check the last history entry's timestamp. The test
                // contract is that `older_than = 0` removes every terminal
                // entry — so the comparison is `<=` rather than `<` to
                // handle the degenerate case where `ts == cutoff` under
                // millisecond resolution.
                let is_old = self
                    .history
                    .get(&id)
                    .and_then(|h| h.value().last().map(|(ts, _)| *ts <= cutoff))
                    .unwrap_or(false);
                if is_old { Some(id) } else { None }
            })
            .collect();

        for id in to_remove {
            self.states.remove(&id);
            self.history.remove(&id);
            purged = purged.saturating_add(1);
        }

        purged
    }

    /// Remove all tracked states. Useful for testing or book reset.
    pub fn clear(&self) {
        self.states.clear();
        self.history.clear();
        if let Ok(mut queue) = self.terminal_queue.lock() {
            queue.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cancel_reason_display() {
        assert_eq!(CancelReason::UserRequested.to_string(), "user requested");
        assert_eq!(
            CancelReason::SelfTradePrevention.to_string(),
            "self-trade prevention"
        );
        assert_eq!(
            CancelReason::InsufficientLiquidity.to_string(),
            "insufficient liquidity"
        );
        assert_eq!(CancelReason::MassCancelAll.to_string(), "mass cancel all");
        assert_eq!(
            CancelReason::MassCancelBySide.to_string(),
            "mass cancel by side"
        );
        assert_eq!(
            CancelReason::MassCancelByUser.to_string(),
            "mass cancel by user"
        );
        assert_eq!(
            CancelReason::MassCancelByPriceRange.to_string(),
            "mass cancel by price range"
        );
        assert_eq!(
            CancelReason::TimeInForceExpired.to_string(),
            "time-in-force expired"
        );
    }

    #[test]
    fn test_order_status_is_terminal() {
        assert!(!OrderStatus::Open.is_terminal());
        assert!(
            !OrderStatus::PartiallyFilled {
                original_quantity: 100,
                filled_quantity: 50
            }
            .is_terminal()
        );
        assert!(
            OrderStatus::Filled {
                filled_quantity: 100
            }
            .is_terminal()
        );
        assert!(
            OrderStatus::Cancelled {
                filled_quantity: 0,
                reason: CancelReason::UserRequested
            }
            .is_terminal()
        );
        assert!(
            OrderStatus::Rejected {
                reason: RejectReason::Other(0)
            }
            .is_terminal()
        );
    }

    #[test]
    fn test_order_status_is_active() {
        assert!(OrderStatus::Open.is_active());
        assert!(
            OrderStatus::PartiallyFilled {
                original_quantity: 100,
                filled_quantity: 50
            }
            .is_active()
        );
        assert!(
            !OrderStatus::Filled {
                filled_quantity: 100
            }
            .is_active()
        );
        assert!(
            !OrderStatus::Cancelled {
                filled_quantity: 0,
                reason: CancelReason::UserRequested
            }
            .is_active()
        );
    }

    #[test]
    fn test_order_status_filled_quantity() {
        assert_eq!(OrderStatus::Open.filled_quantity(), 0);
        assert_eq!(
            OrderStatus::PartiallyFilled {
                original_quantity: 100,
                filled_quantity: 30
            }
            .filled_quantity(),
            30
        );
        assert_eq!(
            OrderStatus::Filled {
                filled_quantity: 100
            }
            .filled_quantity(),
            100
        );
        assert_eq!(
            OrderStatus::Cancelled {
                filled_quantity: 20,
                reason: CancelReason::UserRequested
            }
            .filled_quantity(),
            20
        );
        assert_eq!(
            OrderStatus::Rejected {
                reason: RejectReason::InvalidPrice
            }
            .filled_quantity(),
            0
        );
    }

    #[test]
    fn test_order_status_display() {
        assert_eq!(OrderStatus::Open.to_string(), "Open");
        assert_eq!(
            OrderStatus::PartiallyFilled {
                original_quantity: 100,
                filled_quantity: 30
            }
            .to_string(),
            "PartiallyFilled(30/100)"
        );
        assert_eq!(
            OrderStatus::Filled {
                filled_quantity: 100
            }
            .to_string(),
            "Filled(100)"
        );
        assert_eq!(
            OrderStatus::Cancelled {
                filled_quantity: 0,
                reason: CancelReason::UserRequested
            }
            .to_string(),
            "Cancelled(user requested, filled=0)"
        );
        assert_eq!(
            OrderStatus::Rejected {
                reason: RejectReason::InvalidPrice
            }
            .to_string(),
            "Rejected(invalid price)"
        );
    }

    #[test]
    fn test_tracker_new_is_empty() {
        let tracker = OrderStateTracker::new();
        assert!(tracker.is_empty());
        assert_eq!(tracker.len(), 0);
    }

    #[test]
    fn test_tracker_transition_and_get() {
        let tracker = OrderStateTracker::new();
        let id = Id::new_uuid();

        tracker.transition(id, OrderStatus::Open);
        let status = tracker.get(id);
        assert!(status.is_some());
        assert_eq!(status, Some(OrderStatus::Open));
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn test_tracker_lifecycle_open_to_filled() {
        let tracker = OrderStateTracker::new();
        let id = Id::new_uuid();

        tracker.transition(id, OrderStatus::Open);
        tracker.transition(
            id,
            OrderStatus::PartiallyFilled {
                original_quantity: 100,
                filled_quantity: 50,
            },
        );
        tracker.transition(
            id,
            OrderStatus::Filled {
                filled_quantity: 100,
            },
        );

        let status = tracker.get(id);
        assert_eq!(
            status,
            Some(OrderStatus::Filled {
                filled_quantity: 100
            })
        );
    }

    #[test]
    fn test_tracker_lifecycle_open_to_cancelled() {
        let tracker = OrderStateTracker::new();
        let id = Id::new_uuid();

        tracker.transition(id, OrderStatus::Open);
        tracker.transition(
            id,
            OrderStatus::Cancelled {
                filled_quantity: 0,
                reason: CancelReason::UserRequested,
            },
        );

        let status = tracker.get(id);
        assert!(matches!(status, Some(OrderStatus::Cancelled { .. })));
    }

    #[test]
    fn test_tracker_rejected_order() {
        let tracker = OrderStateTracker::new();
        let id = Id::new_uuid();

        tracker.transition(
            id,
            OrderStatus::Rejected {
                reason: RejectReason::InvalidPrice,
            },
        );

        let status = tracker.get(id);
        assert!(matches!(status, Some(OrderStatus::Rejected { .. })));
    }

    #[test]
    fn test_tracker_unknown_order_returns_none() {
        let tracker = OrderStateTracker::new();
        assert!(tracker.get(Id::new_uuid()).is_none());
    }

    #[test]
    fn test_tracker_retention_evicts_oldest() {
        let tracker = OrderStateTracker::with_capacity(3);

        // Fill up with terminal states
        for _ in 0..5 {
            let id = Id::new_uuid();
            tracker.transition(
                id,
                OrderStatus::Filled {
                    filled_quantity: 100,
                },
            );
        }

        // Only 3 should remain (the most recent ones)
        assert!(tracker.len() <= 3);
    }

    #[test]
    fn test_tracker_active_orders_not_evicted() {
        let tracker = OrderStateTracker::with_capacity(2);
        let active_id = Id::new_uuid();

        // Add an active order
        tracker.transition(active_id, OrderStatus::Open);

        // Add terminal orders to exceed capacity
        for _ in 0..5 {
            let id = Id::new_uuid();
            tracker.transition(
                id,
                OrderStatus::Cancelled {
                    filled_quantity: 0,
                    reason: CancelReason::MassCancelAll,
                },
            );
        }

        // Active order should still be tracked
        assert_eq!(tracker.get(active_id), Some(OrderStatus::Open));
    }

    #[test]
    fn test_tracker_listener_fires_on_transition() {
        let mut tracker = OrderStateTracker::new();
        let transitions = Arc::new(Mutex::new(Vec::new()));
        let transitions_clone = Arc::clone(&transitions);

        tracker.set_listener(Arc::new(move |id, old, new| {
            if let Ok(mut t) = transitions_clone.lock() {
                t.push((id, old.clone(), new.clone()));
            }
        }));

        let id = Id::new_uuid();
        tracker.transition(id, OrderStatus::Open);
        tracker.transition(
            id,
            OrderStatus::Filled {
                filled_quantity: 50,
            },
        );

        let t = transitions.lock();
        assert!(t.is_ok());
        let t = t.unwrap_or_else(|_| panic!("lock"));
        assert_eq!(t.len(), 2);

        // First transition: Open → Open (no prior state, so old == new)
        assert_eq!(t[0].1, OrderStatus::Open);
        assert_eq!(t[0].2, OrderStatus::Open);

        // Second transition: Open → Filled
        assert_eq!(t[1].1, OrderStatus::Open);
        assert_eq!(
            t[1].2,
            OrderStatus::Filled {
                filled_quantity: 50
            }
        );
    }

    #[test]
    fn test_tracker_clear() {
        let tracker = OrderStateTracker::new();
        let id = Id::new_uuid();
        tracker.transition(id, OrderStatus::Open);
        assert!(!tracker.is_empty());

        tracker.clear();
        assert!(tracker.is_empty());
        assert!(tracker.get(id).is_none());
    }

    #[test]
    fn test_tracker_concurrent_access() {
        use std::thread;

        let tracker = Arc::new(OrderStateTracker::new());
        let mut handles = Vec::new();

        for _ in 0..10 {
            let t = Arc::clone(&tracker);
            let handle = thread::spawn(move || {
                for _ in 0..100 {
                    let id = Id::new_uuid();
                    t.transition(id, OrderStatus::Open);
                    t.transition(
                        id,
                        OrderStatus::Filled {
                            filled_quantity: 100,
                        },
                    );
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().expect("thread panicked");
        }

        // 10 threads × 100 orders = 1000 orders, all Filled
        assert_eq!(tracker.len(), 1000);
    }

    #[test]
    fn test_order_status_serde_roundtrip() {
        let statuses = vec![
            OrderStatus::Open,
            OrderStatus::PartiallyFilled {
                original_quantity: 100,
                filled_quantity: 30,
            },
            OrderStatus::Filled {
                filled_quantity: 100,
            },
            OrderStatus::Cancelled {
                filled_quantity: 10,
                reason: CancelReason::SelfTradePrevention,
            },
            OrderStatus::Rejected {
                reason: RejectReason::InvalidPrice,
            },
        ];

        for status in &statuses {
            let json = serde_json::to_string(status);
            assert!(json.is_ok());
            let decoded: Result<OrderStatus, _> = serde_json::from_str(&json.unwrap_or_default());
            assert!(decoded.is_ok());
            assert_eq!(&decoded.unwrap_or(OrderStatus::Open), status);
        }
    }

    #[test]
    fn test_cancel_reason_serde_roundtrip() {
        let reasons = vec![
            CancelReason::UserRequested,
            CancelReason::SelfTradePrevention,
            CancelReason::TimeInForceExpired,
            CancelReason::MassCancelAll,
            CancelReason::MassCancelBySide,
            CancelReason::MassCancelByUser,
            CancelReason::MassCancelByPriceRange,
            CancelReason::InsufficientLiquidity,
        ];

        for reason in &reasons {
            let json = serde_json::to_string(reason);
            assert!(json.is_ok());
            let decoded: Result<CancelReason, _> = serde_json::from_str(&json.unwrap_or_default());
            assert!(decoded.is_ok());
            assert_eq!(&decoded.unwrap_or(CancelReason::UserRequested), reason);
        }
    }
}
