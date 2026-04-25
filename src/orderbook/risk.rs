//! Pre-trade risk layer for `OrderBook<T>`.
//!
//! This module provides the operator-driven, opt-in risk gating for new
//! flow on the order book. It is composed of:
//!
//! - [`RiskConfig`] — the operator-supplied limits (per-account open
//!   orders, per-account notional, price band against a reference price).
//! - [`ReferencePriceSource`] — selects the reference price used by the
//!   price-band check.
//! - [`RiskState`] — bound to an [`OrderBook`](super::book::OrderBook),
//!   carries the optional config plus per-account counters
//!   (`DashMap<Hash32, RiskCounters>`) and per-resting-order entries
//!   (`DashMap<Id, RiskEntry>`). When [`RiskConfig`] is `None`, every
//!   check returns `Ok(())` and every hook is a no-op — the engine pays
//!   only the cost of an `Option::is_none` branch.
//!
//! Check ordering on submit is documented as
//! `kill_switch → risk → STP → fees → match`.
//!
//! ## Decision C
//!
//! Market orders skip every risk check (no submitted price; no rest;
//! no contribution to the resting open-order count). Kill switch still
//! gates them. [`RiskState::check_market_admission`] therefore returns
//! `Ok(())` unconditionally and exists only to keep the gate ordering
//! consistent across submit and add paths and to leave room for a
//! future per-account market-order rate limiter without breaking the
//! call shape.

use crate::orderbook::error::OrderBookError;
use crossbeam::atomic::AtomicCell;
use dashmap::DashMap;
use pricelevel::{Hash32, Id, PriceLevelSnapshot};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing::warn;

/// Source for the reference price used by the price-band check.
///
/// The price band rejects orders whose limit price deviates from the
/// reference by more than the configured number of basis points.
/// `LastTrade` and `Mid` resolve dynamically per check; `FixedPrice`
/// is operator-pinned (e.g. an external mark price piped in).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum ReferencePriceSource {
    /// Last executed trade price. The check is skipped when no trade
    /// has occurred yet on this book.
    LastTrade,
    /// Integer midpoint `(best_bid + best_ask) / 2`. Falls back to
    /// `LastTrade` when the book is one-sided. The check is skipped
    /// when neither a midpoint nor a last trade is available.
    Mid,
    /// Caller-supplied fixed reference price (raw integer ticks). The
    /// check always runs.
    FixedPrice(u128),
}

/// Per-`OrderBook` risk configuration.
///
/// Build via [`RiskConfig::new`] and the chained `with_*` methods. Empty
/// config (every field `None`) is a no-op passthrough — every check
/// returns `Ok(())`. The struct is `Default` and `Serialize`/
/// `Deserialize`, so it round-trips cleanly through the snapshot
/// package with `#[serde(default)]`.
///
/// # Semantics: submitted vs. resting
///
/// The `max_open_orders_per_account` and `max_notional_per_account`
/// limits are evaluated against the **submitted** quantity / notional,
/// **before** matching. An aggressive limit order that would fully
/// match against the opposite side and leave nothing resting is still
/// gated against these limits as if every contract were going to rest.
///
/// This is the standard pre-trade gating pattern in tier-one electronic
/// venues (CME / Nasdaq pre-trade risk hooks behave the same way): the
/// engine does not speculatively match before deciding whether to
/// admit. Counter updates **after** matching reflect the actual resting
/// remainder, so a fully-filled aggressive order does not leave
/// long-lived counter pressure on the account — only the in-flight
/// admission check sees the worst case.
///
/// If you need a "would-rest" projection instead of a "submitted"
/// admission gate, run a `peek_match` simulation in your gateway
/// layer and pass the resulting resting remainder in. Issue a
/// follow-up if you want this surfaced from the engine itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Maximum number of resting orders a single account may have on
    /// this book at any time. `None` disables the check.
    pub max_open_orders_per_account: Option<u64>,
    /// Maximum notional (`price × quantity`, in raw ticks) a single
    /// account may have resting on this book at any time. `None`
    /// disables the check.
    pub max_notional_per_account: Option<u128>,
    /// Maximum allowed deviation in basis points between an incoming
    /// limit price and the resolved reference price. `None` (or
    /// `reference_price = None`) disables the check.
    pub price_band_bps: Option<u32>,
    /// Reference price source used by the price-band check.
    pub reference_price: Option<ReferencePriceSource>,
}

impl RiskConfig {
    /// Construct an empty configuration with every limit disabled.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum number of resting orders per account.
    #[inline]
    #[must_use]
    pub fn with_max_open_orders_per_account(mut self, n: u64) -> Self {
        self.max_open_orders_per_account = Some(n);
        self
    }

    /// Set the maximum resting notional per account (in raw ticks).
    #[inline]
    #[must_use]
    pub fn with_max_notional_per_account(mut self, n: u128) -> Self {
        self.max_notional_per_account = Some(n);
        self
    }

    /// Set the price-band tolerance in basis points and the reference
    /// price source used to evaluate the band.
    #[inline]
    #[must_use]
    pub fn with_price_band_bps(mut self, bps: u32, source: ReferencePriceSource) -> Self {
        self.price_band_bps = Some(bps);
        self.reference_price = Some(source);
        self
    }
}

/// Per-account counters maintained by [`RiskState`].
///
/// Counters are updated with `Relaxed` ordering on the hot path. They
/// are estimative: a transient over- or under-count of one in-flight
/// order is acceptable and does not exceed the configured limit by
/// more than a single race window. Strict accuracy is enforced by
/// snapshot rebuild.
#[derive(Debug, Default)]
pub struct RiskCounters {
    /// Number of resting orders this account currently has on the book.
    pub(super) open_count: AtomicU64,
    /// Sum of `price × remaining_qty` (in raw ticks) across all of
    /// this account's resting orders.
    pub(super) resting_notional: AtomicCell<u128>,
}

/// Per-resting-order risk bookkeeping.
///
/// One entry per order admitted into the resting book. Used on cancel
/// and fill to compute the deltas applied to per-account counters.
#[derive(Debug, Clone, Copy)]
pub(super) struct RiskEntry {
    pub(super) account: Hash32,
    pub(super) price: u128,
    pub(super) remaining_qty: u64,
}

/// Risk state bound to a single [`OrderBook`](super::book::OrderBook).
///
/// Carries the optional [`RiskConfig`], the per-account counters, the
/// per-order entry map, and a one-shot warning latch for the
/// "no reference price available" code path. All public operations
/// are no-ops when `config` is `None`.
#[derive(Debug, Default)]
pub struct RiskState {
    pub(super) config: Option<RiskConfig>,
    pub(super) counters: DashMap<Hash32, RiskCounters>,
    pub(super) orders: DashMap<Id, RiskEntry>,
    pub(super) warned_no_reference: AtomicBool,
}

/// Saturating decrement on an `AtomicU64` via `fetch_update`. Clamps at
/// zero so a double-decrement under a fill / cancel race floors rather
/// than wrapping to `u64::MAX` and permanently locking an account out
/// of admission.
#[inline]
fn saturating_sub_u64(counter: &AtomicU64, delta: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(delta))
    });
}

/// Saturating decrement on an `AtomicCell<u128>` via a compare-exchange
/// loop. Clamps at zero — same rationale as [`saturating_sub_u64`].
#[inline]
fn saturating_sub_u128(cell: &AtomicCell<u128>, delta: u128) {
    let mut current = cell.load();
    loop {
        let new = current.saturating_sub(delta);
        match cell.compare_exchange(current, new) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

impl RiskState {
    /// Construct an empty state with no configuration installed.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install or replace the active risk configuration. Counters and
    /// per-order entries are preserved so that history rebuilt from a
    /// previous configuration remains consistent.
    pub fn set_config(&mut self, cfg: RiskConfig) {
        self.config = Some(cfg);
        self.warned_no_reference.store(false, Ordering::Relaxed);
    }

    /// Read-only access to the active configuration, if any.
    #[inline]
    #[must_use]
    pub fn config(&self) -> Option<&RiskConfig> {
        self.config.as_ref()
    }

    /// Drop the active configuration. Counters and per-order entries
    /// are preserved so a subsequent [`Self::set_config`] re-engages
    /// without dropping history.
    pub fn disable(&mut self) {
        self.config = None;
    }

    /// Pre-trade limit-order admission check.
    ///
    /// Runs three checks in order: per-account open-order count,
    /// per-account notional, and price band. The price-band check is
    /// skipped when `reference_price` is `None` (caller resolved no
    /// reference; e.g. empty book and no trades yet).
    ///
    /// Allocation-free on the happy path. Cold rejection allocates one
    /// error variant.
    #[inline]
    pub(super) fn check_limit_admission(
        &self,
        account: Hash32,
        price: u128,
        quantity: u64,
        reference_price: Option<u128>,
    ) -> Result<(), OrderBookError> {
        let Some(cfg) = self.config.as_ref() else {
            return Ok(());
        };

        // 1. Per-account open-order count.
        if let Some(limit) = cfg.max_open_orders_per_account {
            let current = self
                .counters
                .get(&account)
                .map(|c| c.open_count.load(Ordering::Relaxed))
                .unwrap_or(0);
            if current >= limit {
                return Err(OrderBookError::RiskMaxOpenOrders {
                    account,
                    current,
                    limit,
                });
            }
        }

        // 2. Per-account notional.
        if let Some(limit) = cfg.max_notional_per_account {
            let current = self
                .counters
                .get(&account)
                .map(|c| c.resting_notional.load())
                .unwrap_or(0);
            let attempted = (quantity as u128).saturating_mul(price);
            // Check if `current + attempted` would exceed `limit`.
            if current.saturating_add(attempted) > limit {
                return Err(OrderBookError::RiskMaxNotional {
                    account,
                    current,
                    attempted,
                    limit,
                });
            }
        }

        // 3. Price band against a reference price.
        if let (Some(bps_limit), Some(reference)) = (cfg.price_band_bps, reference_price) {
            // Compute deviation in basis points: |submitted - reference| / reference * 10_000.
            // Use u128 arithmetic to avoid overflow; saturate at u32::MAX.
            if reference > 0 {
                let diff = price.abs_diff(reference);
                // bps = diff * 10_000 / reference
                let bps_u128 = diff.saturating_mul(10_000) / reference;
                let deviation_bps = if bps_u128 > u128::from(u32::MAX) {
                    u32::MAX
                } else {
                    bps_u128 as u32
                };
                if deviation_bps > bps_limit {
                    return Err(OrderBookError::RiskPriceBand {
                        submitted: price,
                        reference,
                        deviation_bps,
                        limit_bps: bps_limit,
                    });
                }
            }
        } else if cfg.price_band_bps.is_some()
            && cfg.reference_price.is_some()
            && reference_price.is_none()
        {
            // Band is configured but no reference is currently
            // available (empty book + no trades). Warn once per book
            // and skip the check.
            if self
                .warned_no_reference
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                warn!(
                    "risk: price-band check configured but no reference price available; \
                     check skipped until a trade or two-sided book establishes a reference"
                );
            }
        }

        Ok(())
    }

    /// Pre-trade market-order admission check.
    ///
    /// Per design decision C, market orders skip every risk check (no
    /// submitted price for the band, no resting contribution for the
    /// open-order or notional counters). This helper exists to keep
    /// the documented gate ordering consistent across submit and add
    /// paths and reserves room for a future per-account market-order
    /// rate limiter without breaking the call shape.
    #[inline]
    pub(super) fn check_market_admission(&self, _account: Hash32) -> Result<(), OrderBookError> {
        Ok(())
    }

    /// Hook on successful admission of a resting order.
    ///
    /// Inserts a [`RiskEntry`] keyed by `order_id` and updates the
    /// per-account counters. Allocation only when a new account
    /// counter is created (first-ever order from that account on this
    /// book) or the per-order map's bucket grows.
    pub(super) fn on_admission(
        &self,
        order_id: Id,
        account: Hash32,
        price: u128,
        remaining_qty: u64,
    ) {
        if self.config.is_none() {
            return;
        }
        self.orders.insert(
            order_id,
            RiskEntry {
                account,
                price,
                remaining_qty,
            },
        );
        let counters = self.counters.entry(account).or_default();
        counters.open_count.fetch_add(1, Ordering::Relaxed);
        let notional_delta = (remaining_qty as u128).saturating_mul(price);
        counters.resting_notional.fetch_add(notional_delta);
    }

    /// Hook per fill against a resting maker order.
    ///
    /// Decrements the maker's `remaining_qty` and the per-account
    /// `resting_notional`. If the maker is fully filled, decrements
    /// `open_count` and removes the entry. No-op when the maker is
    /// not tracked (e.g. risk was disabled when the maker was
    /// admitted, or the entry was already evicted by a prior cancel
    /// in the same submit call).
    ///
    /// Both decrements clamp at zero via saturating CAS — under a
    /// double-fill / fill-cancel race the worst case is a counter
    /// that floors at zero rather than wrapping to `u64::MAX` /
    /// `u128::MAX` and permanently locking the account out of
    /// admission.
    pub(super) fn on_fill(&self, maker_id: Id, filled_qty: u64, maker_price: u128) {
        if self.config.is_none() {
            return;
        }
        // Read-modify-write the entry. Use `get_mut` for the partial
        // case and `remove` for the full case to keep the map small.
        let (account, fully_filled) = {
            let Some(mut entry) = self.orders.get_mut(&maker_id) else {
                return;
            };
            let new_remaining = entry.remaining_qty.saturating_sub(filled_qty);
            let account = entry.account;
            entry.remaining_qty = new_remaining;
            (account, new_remaining == 0)
        };

        let notional_delta = (filled_qty as u128).saturating_mul(maker_price);

        if let Some(counters_ref) = self.counters.get(&account) {
            saturating_sub_u128(&counters_ref.resting_notional, notional_delta);
            if fully_filled {
                saturating_sub_u64(&counters_ref.open_count, 1);
            }
        }

        if fully_filled {
            self.orders.remove(&maker_id);
        }
    }

    /// Hook on cancel of a resting order.
    ///
    /// Removes the entry and decrements both per-account counters
    /// using the entry's stored `remaining_qty` and `price`. No-op
    /// when the entry is not present.
    ///
    /// Both decrements clamp at zero via saturating CAS — same
    /// rationale as [`on_fill`].
    pub(super) fn on_cancel(&self, order_id: Id) {
        if self.config.is_none() {
            return;
        }
        let Some((_, entry)) = self.orders.remove(&order_id) else {
            return;
        };
        let notional_delta = (entry.remaining_qty as u128).saturating_mul(entry.price);
        if let Some(counters_ref) = self.counters.get(&entry.account) {
            saturating_sub_u64(&counters_ref.open_count, 1);
            saturating_sub_u128(&counters_ref.resting_notional, notional_delta);
        }
    }

    /// Rebuild the per-order map and per-account counters by walking
    /// the supplied bid and ask snapshots.
    ///
    /// Called by `OrderBook::restore_from_snapshot_package` after the
    /// snapshot's resting orders have been re-installed into the book.
    /// Iteration is in input-vector order, which is deterministic and
    /// does not affect outbound emissions.
    pub(super) fn rebuild_from_snapshot(
        &self,
        bids: &[PriceLevelSnapshot],
        asks: &[PriceLevelSnapshot],
    ) {
        self.orders.clear();
        self.counters.clear();
        for level in bids.iter().chain(asks.iter()) {
            let price = level.price();
            for order in level.orders() {
                let account = order.user_id();
                let remaining_qty = order
                    .visible_quantity()
                    .saturating_add(order.hidden_quantity());
                self.orders.insert(
                    order.id(),
                    RiskEntry {
                        account,
                        price,
                        remaining_qty,
                    },
                );
                let counters = self.counters.entry(account).or_default();
                counters.open_count.fetch_add(1, Ordering::Relaxed);
                let notional_delta = (remaining_qty as u128).saturating_mul(price);
                counters.resting_notional.fetch_add(notional_delta);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::Id;

    fn account(byte: u8) -> Hash32 {
        Hash32::new([byte; 32])
    }

    #[test]
    fn test_risk_config_builder() {
        let cfg = RiskConfig::new()
            .with_max_open_orders_per_account(5)
            .with_max_notional_per_account(1_000_000)
            .with_price_band_bps(500, ReferencePriceSource::LastTrade);
        assert_eq!(cfg.max_open_orders_per_account, Some(5));
        assert_eq!(cfg.max_notional_per_account, Some(1_000_000));
        assert_eq!(cfg.price_band_bps, Some(500));
        assert_eq!(cfg.reference_price, Some(ReferencePriceSource::LastTrade));
    }

    #[test]
    fn test_risk_state_no_config_is_passthrough() {
        let state = RiskState::new();
        let acct = account(1);
        let order_id = Id::new_uuid();

        // Every check returns Ok.
        assert!(
            state
                .check_limit_admission(acct, 100, 10, Some(100))
                .is_ok()
        );
        assert!(state.check_market_admission(acct).is_ok());

        // Hooks are no-ops.
        state.on_admission(order_id, acct, 100, 10);
        state.on_fill(order_id, 5, 100);
        state.on_cancel(order_id);

        // Counters never populated when no config is installed.
        assert!(state.counters.is_empty());
        assert!(state.orders.is_empty());
    }

    #[test]
    fn test_on_admission_then_on_cancel_round_trip() {
        let mut state = RiskState::new();
        state.set_config(
            RiskConfig::new()
                .with_max_open_orders_per_account(10)
                .with_max_notional_per_account(1_000_000),
        );

        let acct = account(2);
        let order_id = Id::new_uuid();
        state.on_admission(order_id, acct, 100, 10);

        let counters = state
            .counters
            .get(&acct)
            .expect("counters entry created on admission");
        assert_eq!(counters.open_count.load(Ordering::Relaxed), 1);
        assert_eq!(counters.resting_notional.load(), 1_000);
        drop(counters);

        state.on_cancel(order_id);
        let counters = state
            .counters
            .get(&acct)
            .expect("counters entry retained after cancel");
        assert_eq!(counters.open_count.load(Ordering::Relaxed), 0);
        assert_eq!(counters.resting_notional.load(), 0);
        assert!(!state.orders.contains_key(&order_id));
    }

    #[test]
    fn test_on_fill_partial_keeps_open_count() {
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_notional_per_account(1_000_000));

        let acct = account(3);
        let order_id = Id::new_uuid();
        state.on_admission(order_id, acct, 100, 10);

        state.on_fill(order_id, 4, 100);

        let counters = state.counters.get(&acct).expect("counters entry present");
        assert_eq!(
            counters.open_count.load(Ordering::Relaxed),
            1,
            "partial fill must not drop open_count"
        );
        assert_eq!(
            counters.resting_notional.load(),
            6 * 100,
            "notional must be reduced by filled_qty * price"
        );
        let entry = state
            .orders
            .get(&order_id)
            .expect("entry retained after partial fill");
        assert_eq!(entry.remaining_qty, 6);
    }

    #[test]
    fn test_on_fill_full_decrements_open_count() {
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_open_orders_per_account(10));

        let acct = account(4);
        let order_id = Id::new_uuid();
        state.on_admission(order_id, acct, 100, 10);

        state.on_fill(order_id, 10, 100);

        let counters = state.counters.get(&acct).expect("counters entry retained");
        assert_eq!(counters.open_count.load(Ordering::Relaxed), 0);
        assert_eq!(counters.resting_notional.load(), 0);
        assert!(!state.orders.contains_key(&order_id));
    }

    #[test]
    fn test_check_limit_admission_max_open_orders_breach_returns_typed_error() {
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_open_orders_per_account(2));

        let acct = account(5);
        state.on_admission(Id::new_uuid(), acct, 100, 1);
        state.on_admission(Id::new_uuid(), acct, 100, 1);

        let err = state
            .check_limit_admission(acct, 100, 1, Some(100))
            .expect_err("third admission must breach max_open_orders");
        match err {
            OrderBookError::RiskMaxOpenOrders {
                account: a,
                current,
                limit,
            } => {
                assert_eq!(a, acct);
                assert_eq!(current, 2);
                assert_eq!(limit, 2);
            }
            other => panic!("expected RiskMaxOpenOrders, got {other:?}"),
        }
    }

    #[test]
    fn test_check_limit_admission_max_notional_breach_returns_typed_error() {
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_notional_per_account(1_000));

        let acct = account(6);
        // Pre-load 800 of notional.
        state.on_admission(Id::new_uuid(), acct, 100, 8);

        // Attempt to add 300 more (price=100, qty=3).
        let err = state
            .check_limit_admission(acct, 100, 3, Some(100))
            .expect_err("notional should be exceeded");
        match err {
            OrderBookError::RiskMaxNotional {
                account: a,
                current,
                attempted,
                limit,
            } => {
                assert_eq!(a, acct);
                assert_eq!(current, 800);
                assert_eq!(attempted, 300);
                assert_eq!(limit, 1_000);
            }
            other => panic!("expected RiskMaxNotional, got {other:?}"),
        }
    }

    #[test]
    fn test_check_limit_admission_price_band_breach_returns_typed_error() {
        let mut state = RiskState::new();
        // 100 bps = 1% band.
        state.set_config(
            RiskConfig::new().with_price_band_bps(100, ReferencePriceSource::LastTrade),
        );

        let acct = account(7);
        // Reference 1_000_000, submitted 1_100_000 → +10_000 bps deviation.
        let err = state
            .check_limit_admission(acct, 1_100_000, 1, Some(1_000_000))
            .expect_err("price band should be exceeded");
        match err {
            OrderBookError::RiskPriceBand {
                submitted,
                reference,
                deviation_bps,
                limit_bps,
            } => {
                assert_eq!(submitted, 1_100_000);
                assert_eq!(reference, 1_000_000);
                assert_eq!(deviation_bps, 1_000); // 10% = 1_000 bps
                assert_eq!(limit_bps, 100);
            }
            other => panic!("expected RiskPriceBand, got {other:?}"),
        }
    }

    #[test]
    fn test_check_limit_admission_no_reference_price_skips_band_check() {
        let mut state = RiskState::new();
        state.set_config(
            RiskConfig::new().with_price_band_bps(100, ReferencePriceSource::LastTrade),
        );
        // No reference available. Check skipped → Ok.
        assert!(
            state
                .check_limit_admission(account(8), 999_999_999, 1, None)
                .is_ok()
        );
    }

    #[test]
    fn test_check_limit_admission_warns_only_once_when_no_reference_available() {
        let mut state = RiskState::new();
        state.set_config(
            RiskConfig::new().with_price_band_bps(100, ReferencePriceSource::LastTrade),
        );

        let acct = account(9);
        assert!(state.check_limit_admission(acct, 1, 1, None).is_ok());
        assert!(
            state.warned_no_reference.load(Ordering::Relaxed),
            "first call without reference should flip the latch"
        );
        // Second call: latch already set; check still passes, no
        // additional warning emitted (we cannot assert log count here
        // without a tracing-subscriber harness, but the latch is the
        // gate on the log site).
        assert!(state.check_limit_admission(acct, 2, 2, None).is_ok());
        assert!(state.warned_no_reference.load(Ordering::Relaxed));
    }

    #[test]
    fn test_within_limits_admission_succeeds() {
        let mut state = RiskState::new();
        state.set_config(
            RiskConfig::new()
                .with_max_open_orders_per_account(10)
                .with_max_notional_per_account(1_000_000)
                .with_price_band_bps(500, ReferencePriceSource::LastTrade),
        );

        let acct = account(10);
        // Reference 100, submitted 100 → 0 bps. All checks pass.
        assert!(state.check_limit_admission(acct, 100, 5, Some(100)).is_ok());
    }

    #[test]
    fn test_disable_keeps_counters() {
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_open_orders_per_account(10));

        let acct = account(11);
        let order_id = Id::new_uuid();
        state.on_admission(order_id, acct, 100, 10);

        state.disable();

        // Config gone, but counters remain.
        assert!(state.config().is_none());
        assert!(state.counters.contains_key(&acct));
        assert!(state.orders.contains_key(&order_id));

        // After disable, every check is a passthrough again.
        assert!(
            state
                .check_limit_admission(acct, 100, 100, Some(100))
                .is_ok()
        );
    }

    #[test]
    fn test_on_fill_overshoot_clamps_counters_at_zero() {
        // Regression: a stray double-fill or filled_qty > remaining
        // must not wrap counters via `fetch_sub`. Both decrements
        // saturate at zero.
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_notional_per_account(10_000));

        let acct = account(12);
        let order_id = Id::new_uuid();
        state.on_admission(order_id, acct, 100, 5);

        // Decrement by far more than what was admitted.
        state.on_fill(order_id, 1_000_000, 100);

        let counters = state.counters.get(&acct).expect("counters present");
        assert_eq!(counters.open_count.load(Ordering::Relaxed), 0);
        assert_eq!(counters.resting_notional.load(), 0);
    }

    #[test]
    fn test_on_cancel_after_fully_filled_is_noop_and_does_not_wrap() {
        // Regression: cancel after the entry has already been removed
        // by an on_fill must be a no-op and not under-flow the
        // counters that the prior fill already drove to zero.
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_open_orders_per_account(10));

        let acct = account(13);
        let order_id = Id::new_uuid();
        state.on_admission(order_id, acct, 100, 5);
        state.on_fill(order_id, 5, 100); // entry removed, counters at 0
        state.on_cancel(order_id); // no-op (entry not present)

        let counters = state.counters.get(&acct).expect("counters present");
        assert_eq!(counters.open_count.load(Ordering::Relaxed), 0);
        assert_eq!(counters.resting_notional.load(), 0);
    }
}
