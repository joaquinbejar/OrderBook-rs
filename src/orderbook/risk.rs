//! Pre-trade risk layer for `OrderBook<T>`.
//!
//! This module provides the operator-driven, opt-in risk gating for new
//! flow on the order book. It is composed of:
//!
//! - [`RiskConfig`] — the operator-supplied limits (per-account open
//!   orders, per-account notional, price band against a reference price).
//! - [`ReferencePriceSource`] — selects the reference price used by the
//!   price-band check.
//! - [`RiskState`] — bound to an [`OrderBook`](crate::OrderBook),
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
//! gates them. `RiskState::check_market_admission` therefore returns
//! `Ok(())` unconditionally and exists only to keep the gate ordering
//! consistent across submit and add paths and to leave room for a
//! future per-account market-order rate limiter without breaking the
//! call shape.

use crate::orderbook::error::OrderBookError;
use crossbeam::atomic::AtomicCell;
use dashmap::DashMap;
use pricelevel::{Hash32, Id};
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

/// Risk state bound to a single [`OrderBook`](crate::OrderBook).
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
        self.check_price_band(cfg, price, reference_price)?;

        Ok(())
    }

    /// Price-band check shared by [`Self::check_limit_admission`] and
    /// [`Self::check_modify_admission`].
    ///
    /// Rejects when the deviation of `price` from the resolved
    /// `reference_price` *strictly* exceeds `cfg.price_band_bps`. The
    /// comparison cross-multiplies (`diff * 10_000` vs.
    /// `bps_limit * reference`) instead of dividing so the band never
    /// under-enforces: truncating integer division would floor the bps,
    /// letting an order whose true deviation is fractionally above the
    /// band round down to the limit and slip through (#113). An order
    /// exactly at the limit is admitted, preserving the original
    /// strict-`>` boundary semantics. `u128` throughout with saturation.
    ///
    /// Skips silently (warning once per book) when the band is configured
    /// but no reference price is currently available.
    #[inline]
    fn check_price_band(
        &self,
        cfg: &RiskConfig,
        price: u128,
        reference_price: Option<u128>,
    ) -> Result<(), OrderBookError> {
        if let (Some(bps_limit), Some(reference)) = (cfg.price_band_bps, reference_price) {
            if reference > 0 {
                let diff = price.abs_diff(reference);
                let scaled_diff = diff.saturating_mul(10_000);
                let band = u128::from(bps_limit).saturating_mul(reference);
                if scaled_diff > band {
                    // Recompute the floored bps only for the error payload display.
                    let bps_u128 = scaled_diff / reference;
                    let deviation_bps = if bps_u128 > u128::from(u32::MAX) {
                        u32::MAX
                    } else {
                        bps_u128 as u32
                    };
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

    /// Pre-trade admission check for an in-place **modify** of a resting
    /// order (`UpdatePrice` / `UpdatePriceAndQuantity` / `Replace`).
    ///
    /// A modify replaces one resting order with another: the account's
    /// `open_count` is unchanged (one out, one in) and — critically — the
    /// *original* order's contribution is still counted in the account's
    /// counters at the moment this runs (the validate-first guard checks
    /// admission *before* cancelling, #98). Reusing
    /// [`Self::check_limit_admission`] here would therefore double-count
    /// the original and falsely reject. This check instead:
    ///
    /// - runs the **price band** on `new_price` (same logic as the
    ///   limit-admission band, via [`Self::check_price_band`]),
    /// - runs the **notional** check against `max_notional_per_account`
    ///   using the *projected* resting notional
    ///   `current - old_price*old_qty + new_price*new_qty` (the old
    ///   order's contribution is already inside `current`), with `u128`
    ///   saturating arithmetic,
    /// - does **not** check `max_open_orders_per_account` (a modify cannot
    ///   change the resting order count).
    ///
    /// Returns `Ok(())` when no [`RiskConfig`] is installed.
    ///
    /// # Errors
    /// Returns [`OrderBookError::RiskMaxNotional`] or
    /// [`OrderBookError::RiskPriceBand`] when the projected modify would
    /// breach the corresponding limit.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_modify_admission(
        &self,
        order_id: Id,
        account: Hash32,
        new_price: u128,
        new_qty: u64,
        reference_price: Option<u128>,
    ) -> Result<(), OrderBookError> {
        let Some(cfg) = self.config.as_ref() else {
            return Ok(());
        };

        // Look up the original order's tracked risk contribution. If it is NOT
        // tracked — admitted while no `RiskConfig` was installed, then a config
        // was installed before this modify — the modify is, from the risk
        // layer's view, a genuinely new admission: `on_cancel` will be a no-op
        // for the untracked original and `add_order` runs FULL admission
        // post-cancel. Mirror that exactly (full `check_limit_admission`,
        // including the open-order count) so the validate-first guard predicts
        // the post-cancel verdict and never passes a modify that `add_order`
        // would then reject — which would destroy the original.
        let old_contribution = {
            let Some(entry) = self.orders.get(&order_id) else {
                return self.check_limit_admission(account, new_price, new_qty, reference_price);
            };
            u128::from(entry.remaining_qty).saturating_mul(entry.price)
        };

        // Tracked original: a modify is net one-out-one-in, so `open_count` is
        // unchanged (skip that gate) and only the notional and price band can
        // newly breach. Project the account's resting notional by swapping the
        // original's contribution (already inside `current`) for the new one.
        // Saturating throughout: a transient under-count floors at zero.
        if let Some(limit) = cfg.max_notional_per_account {
            let current = self
                .counters
                .get(&account)
                .map(|c| c.resting_notional.load())
                .unwrap_or(0);
            let new_contribution = (new_qty as u128).saturating_mul(new_price);
            let projected = current
                .saturating_sub(old_contribution)
                .saturating_add(new_contribution);
            if projected > limit {
                return Err(OrderBookError::RiskMaxNotional {
                    account,
                    current,
                    attempted: new_contribution,
                    limit,
                });
            }
        }

        // Price band against the reference price on the new limit price.
        self.check_price_band(cfg, new_price, reference_price)?;

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
    /// `open_count`, removes the per-order entry, and evicts the
    /// per-account counters when the account drops to zero resting
    /// orders and zero notional (see [`Self::evict_if_zeroed`]). No-op
    /// when the maker is not tracked (e.g. risk was disabled when the
    /// maker was admitted, or the entry was already evicted by a prior
    /// cancel in the same submit call).
    ///
    /// `resting_notional` is reduced using the maker's **stored
    /// admission price** (`RiskEntry::price`), not the passed
    /// `maker_price`. The account's resting exposure was booked at the
    /// admission price, so admission / fill / cancel stay self-balancing
    /// regardless of the execution price. `maker_price` is kept only as
    /// a debug tripwire: today the matcher always trades a maker at its
    /// resting price (and a modify / repricing re-admits a fresh entry
    /// at the new price), so the two coincide; a future
    /// price-improvement path that breaks that equality must revisit
    /// this accounting.
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
        let (account, entry_price, fully_filled) = {
            let Some(mut entry) = self.orders.get_mut(&maker_id) else {
                return;
            };
            let new_remaining = entry.remaining_qty.saturating_sub(filled_qty);
            let account = entry.account;
            let entry_price = entry.price;
            entry.remaining_qty = new_remaining;
            (account, entry_price, new_remaining == 0)
        };

        // Self-balancing: release the filled portion at the admission
        // price the notional was booked at. See the method docs for why
        // `maker_price` is only an assertion.
        debug_assert_eq!(
            maker_price, entry_price,
            "a maker fills at its resting price today; revisit resting_notional accounting before adding price improvement"
        );
        let notional_delta = (filled_qty as u128).saturating_mul(entry_price);

        if let Some(counters_ref) = self.counters.get(&account) {
            saturating_sub_u128(&counters_ref.resting_notional, notional_delta);
            if fully_filled {
                saturating_sub_u64(&counters_ref.open_count, 1);
            }
        }
        // `counters_ref` (a read guard on the counters shard) is dropped at
        // the closing brace above, BEFORE `evict_if_zeroed` takes the write
        // guard on the same shard. DashMap shards are non-reentrant, so this
        // ordering matters: do not widen the read-guard scope across the
        // eviction call or it self-deadlocks.
        if fully_filled {
            self.orders.remove(&maker_id);
            self.evict_if_zeroed(account);
        }
    }

    /// Atomically evict an account's [`RiskCounters`] once it has no
    /// resting orders and zero resting notional, so the per-account map
    /// tracks currently-active accounts instead of growing with every
    /// distinct account ever seen.
    ///
    /// Race-safe against [`Self::on_admission`]. `remove_if` evaluates the
    /// predicate while holding the counters shard's write lock, and
    /// `on_admission` holds that *same* lock across its whole
    /// `entry(account).or_default()` plus the `open_count` /
    /// `resting_notional` increments — the `RefMut` is bound for the rest
    /// of that call — so this never observes a half-incremented counter.
    /// The two serialize: either the admission commits first and the
    /// predicate reads a non-zero `open_count` and keeps the entry, or the
    /// eviction commits first and the admission recreates the entry from
    /// zero. Eviction therefore reliably reclaims any account that reaches
    /// a genuine zero-resting state (the decrement that zeroes the account
    /// is the same call that attempts the eviction) without ever evicting
    /// an account that still has — or is concurrently regaining — a
    /// resting order.
    #[inline]
    fn evict_if_zeroed(&self, account: Hash32) {
        self.counters.remove_if(&account, |_, c| {
            c.open_count.load(Ordering::Relaxed) == 0 && c.resting_notional.load() == 0
        });
    }

    /// Hook on cancel of a resting order.
    ///
    /// Removes the entry and decrements both per-account counters
    /// using the entry's stored `remaining_qty` and `price`, then
    /// evicts the per-account counters when the account drops to zero
    /// resting orders and zero notional (see [`Self::evict_if_zeroed`]).
    /// No-op when the entry is not present.
    ///
    /// Both decrements clamp at zero via saturating CAS — same
    /// rationale as \[`on_fill`\].
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
        // `counters_ref` read guard dropped above before the write guard in
        // `evict_if_zeroed` — same non-reentrant shard, must not overlap.
        self.evict_if_zeroed(entry.account);
    }

    /// Drop all per-order risk entries and per-account counters in one shot.
    ///
    /// Used by [`OrderBook::cancel_all_orders`](super::book::OrderBook::cancel_all_orders),
    /// which empties the entire book in bulk — the per-order [`Self::on_cancel`]
    /// accounting collapses to a single clear, and leaving the maps populated would
    /// strand phantom open-order / notional counters that reject new flow (#99).
    /// No-op semantics when no `RiskConfig` is installed (the maps are already empty).
    pub(super) fn clear(&self) {
        self.orders.clear();
        self.counters.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::Id;

    fn account(byte: u8) -> Hash32 {
        Hash32::new([byte; 32])
    }

    fn open_count_of(state: &RiskState, acct: Hash32) -> u64 {
        state
            .counters
            .get(&acct)
            .map(|c| c.open_count.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    #[test]
    fn test_concurrent_admission_over_admission_is_bounded_issue_116() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        const THREADS: usize = 16;
        const LIMIT: u64 = 4;

        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_open_orders_per_account(LIMIT));
        let state = Arc::new(state);
        let acct = account(7);
        // Barrier releases all threads together to maximize the documented
        // check-then-increment race window.
        let barrier = Arc::new(Barrier::new(THREADS));

        let handles: Vec<_> = (0..THREADS)
            .map(|i| {
                let state = Arc::clone(&state);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    if state.check_limit_admission(acct, 100, 1, Some(100)).is_ok() {
                        state.on_admission(Id::from_u64(i as u64), acct, 100, 1);
                        1u64
                    } else {
                        0
                    }
                })
            })
            .collect();

        let admitted: u64 = handles
            .into_iter()
            .map(|h| h.join().expect("admission thread"))
            .sum();

        let open_count = open_count_of(&state, acct);

        // The counter must equal the number of successful admissions.
        assert_eq!(
            open_count, admitted,
            "open_count must match the admissions that incremented it"
        );
        // A reject can only happen once the count reaches the limit, so at least
        // `LIMIT` admissions always occur.
        assert!(open_count >= LIMIT, "at least the limit is admitted");
        // Documented bound: over-admission never exceeds the limit by more than
        // one in-flight admission per racing thread.
        assert!(
            open_count <= LIMIT + THREADS as u64,
            "over-admission must stay bounded by limit + thread_count, got {open_count}"
        );
    }

    #[test]
    fn test_concurrent_fill_cancel_never_wraps_open_count_issue_116() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        const ORDERS: u64 = 32;

        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_open_orders_per_account(10_000));
        let acct = account(9);
        // Pre-admit ORDERS resting orders (open_count == ORDERS).
        for i in 0..ORDERS {
            state.on_admission(Id::from_u64(i), acct, 100, 10);
        }
        assert_eq!(open_count_of(&state, acct), ORDERS);

        let state = Arc::new(state);
        // Race a full fill against a cancel for every order: the saturating
        // decrement must never wrap `open_count` to a huge value (which would
        // lock the account out by reading as "at limit" forever).
        let barrier = Arc::new(Barrier::new((ORDERS * 2) as usize));
        let mut handles = Vec::new();
        for i in 0..ORDERS {
            for which in 0..2u8 {
                let state = Arc::clone(&state);
                let barrier = Arc::clone(&barrier);
                handles.push(thread::spawn(move || {
                    barrier.wait();
                    if which == 0 {
                        state.on_fill(Id::from_u64(i), 10, 100); // full fill
                    } else {
                        state.on_cancel(Id::from_u64(i));
                    }
                }));
            }
        }
        for h in handles {
            h.join().expect("fill/cancel thread");
        }

        let open_count = open_count_of(&state, acct);
        let resting_notional = state
            .counters
            .get(&acct)
            .map(|c| c.resting_notional.load())
            .unwrap_or(0);

        // Each order is decremented exactly once (whichever of fill/cancel wins
        // the DashMap entry removal; the other is a no-op), and a saturating
        // decrement never wraps — so both counters land at 0, never at a huge
        // wrapped value that would lock the account out.
        assert_eq!(open_count, 0, "all orders removed exactly once; no wrap");
        assert_eq!(
            resting_notional, 0,
            "resting_notional also reaches 0 without wrap"
        );
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
        // #115: the per-account counters entry is evicted once the account's
        // last resting order is removed (open_count and resting_notional both 0),
        // rather than lingering at zero and growing the map monotonically.
        assert!(
            state.counters.get(&acct).is_none(),
            "counters entry evicted after the account's last order is cancelled"
        );
        assert!(state.counters.is_empty());
        assert!(!state.orders.contains_key(&order_id));
    }

    #[test]
    fn test_on_fill_full_evicts_counters_issue_115() {
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_notional_per_account(1_000_000));

        let acct = account(4);
        let order_id = Id::new_uuid();
        state.on_admission(order_id, acct, 100, 10);

        // Fully fill the account's only resting order: the per-order entry and
        // the now-zeroed per-account counters are both removed.
        state.on_fill(order_id, 10, 100);

        assert!(
            state.counters.get(&acct).is_none(),
            "counters entry evicted after the account's last order is fully filled"
        );
        assert!(state.counters.is_empty());
        assert!(!state.orders.contains_key(&order_id));
    }

    #[test]
    fn test_admission_fill_cancel_notional_self_balances_issue_115() {
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_notional_per_account(1_000_000));

        let acct = account(5);
        let order_id = Id::new_uuid();
        // Admit 10 @ 100 → resting_notional 1_000.
        state.on_admission(order_id, acct, 100, 10);
        assert_eq!(
            state
                .counters
                .get(&acct)
                .map(|c| c.resting_notional.load())
                .unwrap_or(0),
            1_000
        );

        // Partial fill 4 @ 100 releases 400 at the entry's stored admission
        // price → resting_notional 600, open_count still 1 (entry retained).
        state.on_fill(order_id, 4, 100);
        let counters = state
            .counters
            .get(&acct)
            .expect("entry retained on partial");
        assert_eq!(counters.open_count.load(Ordering::Relaxed), 1);
        assert_eq!(counters.resting_notional.load(), 600);
        drop(counters);

        // Cancel the remaining 6 @ 100 releases the last 600 → both counters
        // reach 0 and the account entry is evicted. Admission/fill/cancel
        // self-balance to exactly zero with no residual notional.
        state.on_cancel(order_id);
        assert!(
            state.counters.get(&acct).is_none(),
            "counters self-balance to zero and evict after the last release"
        );
        assert!(state.orders.is_empty());
    }

    #[test]
    fn test_concurrent_admission_vs_eviction_is_consistent_issue_115() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        // Race a full-fill eviction of order A against a fresh admission of
        // order B on the SAME account, repeatedly, to exercise the
        // `evict_if_zeroed` / `on_admission` interleaving on the shared
        // counters shard. The ground-truth invariant that must always hold:
        // `open_count` equals the number of the account's orders still in the
        // per-order map. The eviction must never strand the account by
        // dropping B's increment (phantom under-count) nor wrap a counter.
        const ROUNDS: usize = 500;
        let acct = account(21);
        let a = Id::from_u64(1);
        let b = Id::from_u64(2);

        for round in 0..ROUNDS {
            let mut state = RiskState::new();
            state.set_config(RiskConfig::new().with_max_open_orders_per_account(10_000));
            // Pre-admit A so the account sits at open_count == 1.
            state.on_admission(a, acct, 100, 1);
            let state = Arc::new(state);
            let barrier = Arc::new(Barrier::new(2));

            let (s1, b1) = (Arc::clone(&state), Arc::clone(&barrier));
            let t1 = thread::spawn(move || {
                b1.wait();
                s1.on_fill(a, 1, 100); // full fill of A → attempts eviction
            });
            let (s2, b2) = (Arc::clone(&state), Arc::clone(&barrier));
            let t2 = thread::spawn(move || {
                b2.wait();
                s2.on_admission(b, acct, 100, 1); // concurrent admission of B
            });
            t1.join().expect("fill thread");
            t2.join().expect("admission thread");

            // A is gone, B rests — regardless of who won the race.
            assert!(
                !state.orders.contains_key(&a),
                "round {round}: A fully filled"
            );
            assert!(
                state.orders.contains_key(&b),
                "round {round}: B's entry survives"
            );

            // open_count must equal the account's live resting-order count.
            // B is the only resting order, so this is exactly 1; never 0 (a lost
            // increment / phantom eviction) and never a wrapped value.
            let resting = state.orders.iter().filter(|e| e.account == acct).count() as u64;
            assert_eq!(
                open_count_of(&state, acct),
                resting,
                "round {round}: open_count must track the live resting-order count, never under/overcount"
            );

            // Drain B: the account fully zeroes and the entry is evicted.
            state.on_cancel(b);
            assert!(
                state.counters.get(&acct).is_none(),
                "round {round}: account evicted once its last order is removed"
            );
            assert!(
                state.orders.is_empty(),
                "round {round}: no stranded entries"
            );
        }
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
        let keep = Id::new_uuid();
        let fill = Id::new_uuid();
        // Two resting orders for the account. Fully filling one decrements
        // open_count by exactly one; the entry is retained because the
        // account still has a resting order (eviction needs both counters at 0).
        state.on_admission(keep, acct, 100, 10);
        state.on_admission(fill, acct, 100, 10);

        state.on_fill(fill, 10, 100);

        let counters = state
            .counters
            .get(&acct)
            .expect("entry retained while the account still has a resting order");
        assert_eq!(counters.open_count.load(Ordering::Relaxed), 1);
        assert_eq!(counters.resting_notional.load(), 1_000);
        assert!(!state.orders.contains_key(&fill));
        assert!(state.orders.contains_key(&keep));
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
    fn test_check_limit_admission_price_band_fractional_bps_is_rejected() {
        let mut state = RiskState::new();
        state.set_config(
            RiskConfig::new().with_price_band_bps(100, ReferencePriceSource::LastTrade),
        );
        let acct = account(11);

        // Reference 30_000, limit 100 bps → the band edge is exactly 30_300
        // (100 bps = 300 ticks). 30_301 is 100.33 bps: truncating division
        // floored this to 100 and admitted it; cross-multiplication rejects it.
        match state.check_limit_admission(acct, 30_301, 1, Some(30_000)) {
            Err(OrderBookError::RiskPriceBand {
                deviation_bps,
                limit_bps,
                ..
            }) => {
                assert_eq!(limit_bps, 100);
                assert_eq!(deviation_bps, 100, "display still shows the floored bps");
            }
            other => panic!("fractional over-band order must be rejected, got {other:?}"),
        }

        // An order exactly at the band edge (30_300 = 100.0 bps) is admitted —
        // the strict-`>` boundary semantics are preserved.
        assert!(
            state
                .check_limit_admission(acct, 30_300, 1, Some(30_000))
                .is_ok(),
            "exact-limit order must still be admitted"
        );

        // And just inside the band (30_299) is admitted.
        assert!(
            state
                .check_limit_admission(acct, 30_299, 1, Some(30_000))
                .is_ok()
        );
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

        // Both counters saturate to zero (never wrap) and the account entry is
        // evicted. Eviction is itself the no-wrap proof: a wrapped counter would
        // read as a huge non-zero value and the eviction predicate would retain it.
        assert!(
            state.counters.get(&acct).is_none(),
            "overshoot fill saturates to zero and evicts; a wrap would leave a non-zero count and retain the entry"
        );
        assert!(state.orders.is_empty());
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
        state.on_fill(order_id, 5, 100); // entry removed, counters evicted at 0
        state.on_cancel(order_id); // no-op (entry not present)

        // The full fill drove both counters to zero and evicted the entry; the
        // trailing cancel finds no entry, so it cannot underflow the counters.
        assert!(
            state.counters.get(&acct).is_none(),
            "fill evicted the zeroed entry; the later cancel is a no-op and cannot wrap"
        );
        assert!(state.orders.is_empty());
    }

    // ───────────────────────────────────────────────────────────────
    // Modify-aware admission (#98)
    // ───────────────────────────────────────────────────────────────

    #[test]
    fn test_check_modify_admission_no_config_is_passthrough() {
        let state = RiskState::new();
        assert!(
            state
                .check_modify_admission(Id::new_uuid(), account(1), 999_999, 999, Some(100))
                .is_ok()
        );
    }

    #[test]
    fn test_check_modify_admission_ignores_open_order_count() {
        // A modify of a TRACKED order must never reject on the open-order
        // count: an account sitting exactly at the limit can still modify a
        // resting order (count is net unchanged).
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_open_orders_per_account(1));
        let acct = account(20);
        let id = Id::new_uuid();
        state.on_admission(id, acct, 100, 10); // account at the limit

        assert!(
            state
                .check_modify_admission(id, acct, 110, 10, Some(105))
                .is_ok(),
            "modify of a tracked order must not be gated by max_open_orders_per_account"
        );
    }

    #[test]
    fn test_check_modify_admission_projects_notional_swapping_old_for_new() {
        // Notional ceiling 1_000. Original order contributes 100*8 = 800.
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_notional_per_account(1_000));
        let acct = account(21);
        let id = Id::new_uuid();
        state.on_admission(id, acct, 100, 8); // resting_notional = 800

        // Modify to 100*9 = 900 projects to 800 - 800 + 900 = 900 ≤ 1_000.
        assert!(
            state
                .check_modify_admission(id, acct, 100, 9, Some(100))
                .is_ok(),
            "projected notional 900 must be within the 1_000 ceiling"
        );

        // Modify to 100*11 = 1_100 projects to 800 - 800 + 1_100 = 1_100 > 1_000.
        match state.check_modify_admission(id, acct, 100, 11, Some(100)) {
            Err(OrderBookError::RiskMaxNotional {
                account: a,
                attempted,
                limit,
                ..
            }) => {
                assert_eq!(a, acct);
                assert_eq!(attempted, 1_100);
                assert_eq!(limit, 1_000);
            }
            other => panic!("expected RiskMaxNotional, got {other:?}"),
        }
    }

    #[test]
    fn test_check_modify_admission_projection_does_not_double_count_original() {
        // Regression: the naive limit-admission check would add the new
        // contribution on top of the (still-counted) original and falsely
        // reject. The projection subtracts the original's tracked contribution.
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_notional_per_account(1_000));
        let acct = account(22);
        let id = Id::new_uuid();
        state.on_admission(id, acct, 100, 10); // resting_notional = 1_000 (at ceiling)

        // Re-price to the same notional: 1_000 - 1_000 + 1_000 = 1_000 ≤ 1_000.
        assert!(
            state
                .check_modify_admission(id, acct, 200, 5, Some(150))
                .is_ok(),
            "an unchanged-notional modify must not double-count the original"
        );
    }

    #[test]
    fn test_check_modify_admission_price_band_on_new_price() {
        let mut state = RiskState::new();
        state.set_config(
            RiskConfig::new().with_price_band_bps(100, ReferencePriceSource::LastTrade),
        );
        let acct = account(23);
        let id = Id::new_uuid();
        state.on_admission(id, acct, 1_000_000, 1);

        // New price 1_100_000 vs reference 1_000_000 → +1_000 bps, far over band.
        match state.check_modify_admission(id, acct, 1_100_000, 1, Some(1_000_000)) {
            Err(OrderBookError::RiskPriceBand {
                submitted,
                reference,
                limit_bps,
                ..
            }) => {
                assert_eq!(submitted, 1_100_000);
                assert_eq!(reference, 1_000_000);
                assert_eq!(limit_bps, 100);
            }
            other => panic!("expected RiskPriceBand, got {other:?}"),
        }

        // A new price inside the band is admitted.
        assert!(
            state
                .check_modify_admission(id, acct, 1_005_000, 1, Some(1_000_000))
                .is_ok()
        );
    }

    #[test]
    fn test_check_modify_admission_untracked_original_runs_full_admission() {
        // If the original order has no RiskEntry (admitted while no RiskConfig
        // was installed, then a config was installed before this modify), the
        // modify is a genuinely new admission from the risk layer's view — full
        // admission applies, INCLUDING the open-order count. This mirrors
        // `add_order`'s post-cancel check so the validate-first guard predicts
        // the post-cancel verdict and never destroys the original.
        let mut state = RiskState::new();
        state.set_config(RiskConfig::new().with_max_open_orders_per_account(1));
        let acct = account(24);
        // One OTHER tracked resting order already at the limit.
        state.on_admission(Id::new_uuid(), acct, 100, 10);

        // The order being modified is NOT tracked → full admission → rejected
        // on the open-order count (would be a 2nd order for the account).
        let untracked = Id::new_uuid();
        match state.check_modify_admission(untracked, acct, 110, 5, Some(105)) {
            Err(OrderBookError::RiskMaxOpenOrders { .. }) => {}
            other => panic!(
                "untracked modify must run full admission and reject on open count, got {other:?}"
            ),
        }
    }
}
