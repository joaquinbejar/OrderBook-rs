//! Contains the core matching engine logic for the order book.
//!
//! The matching engine supports Self-Trade Prevention (STP) when configured
//! via [`crate::STPMode`]. When STP is disabled (`STPMode::None`, the default),
//! the matching hot path is unchanged with zero overhead.

use crate::orderbook::order_state::{CancelReason, OrderStatus};
use crate::orderbook::pool::MatchingPool;
use crate::orderbook::reject_reason::RejectReason;
use crate::orderbook::stp::{STPAction, capped_depth_add, check_stp_at_level};
use crate::{OrderBook, OrderBookError};
use either::Either;
use pricelevel::{
    CapacityResource, Hash32, Id, MatchResult, OrderType, PriceLevelError, Quantity, Side,
    TakerKind, TimeInForce,
};
use std::sync::atomic::Ordering;

// Reusable sweep scratch buffers (#107, #230). Module-level so the sweep and
// its early-exit helpers share one pool per thread.
thread_local! {
    static MATCHING_POOL: MatchingPool = MatchingPool::new();
}

/// Result-buffer reservation a sweep makes before it touches its first
/// level (#240, split per vector since PriceLevel#219): room for `trades`
/// more trades and `filled_ids` more filled-maker ids in the aggregate
/// result and the pooled filled-maker buffer. [`Self::NONE`] for every
/// sweep but a fill-or-kill one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SweepReservation {
    /// Trades to reserve room for.
    pub(crate) trades: u64,
    /// Filled-maker ids to reserve room for.
    pub(crate) filled_ids: u64,
}

impl SweepReservation {
    /// No up-front reservation: each level reserves before it is touched.
    pub(crate) const NONE: Self = Self {
        trades: 0,
        filled_ids: 0,
    };

    /// `true` when there is nothing to reserve up front.
    #[inline]
    #[must_use]
    pub(crate) fn is_none(&self) -> bool {
        self.trades == 0 && self.filled_ids == 0
    }
}

/// Resources a fill-or-kill taker's sweep will draw on, measured by the
/// feasibility walk [`OrderBook::fok_fillable_quantity`] before anything is
/// mutated (#240, made exact by PriceLevel#218).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FokFeasibility {
    /// Quantity the sweep would fill, in quantity units.
    pub(crate) fillable: u64,
    /// Trade ids the sweep draws from the book's `UuidGenerator`. Exact
    /// (`MatchRequirements::trade_ids_required`) at every level pricelevel
    /// can dry-run as the sweep will match it; at a level where self-trade
    /// prevention cancels same-user makers first (`CancelMaker`) the dry
    /// run cannot see the post-cancel queue, so the level contributes an
    /// upper bound instead: `min(makers, quantity taken there)` without
    /// hidden depth (every maker trades at most once), the quantity taken
    /// with hidden depth (a replenishing maker can trade again).
    pub(crate) trade_ids: u64,
    /// The up-front result-buffer reservation that covers every level's
    /// fold, so no level can need more room once the first one mutated:
    /// per level the larger of its exact trade count and the per-level
    /// reservation the sweep makes before touching it
    /// (`min(makers, quantity the level is asked for)`). See
    /// `OrderBook::reserve_level_fold`.
    pub(crate) reservation: SweepReservation,
}

/// What the feasibility walk is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeasibilityScope {
    /// The fill-or-kill preflight (#240): every level the sweep will reach
    /// must be matchable in full. A poisoned level (PriceLevel#217), a
    /// per-level counter without headroom (PriceLevel#218) or a maker step
    /// the sweep would stop at fails the walk with that level's error, so
    /// the taker is rejected untouched. Valid only under the exclusive
    /// submit gate.
    Preflight,
    /// A depth estimate only ([`OrderBook::check_modify_reserve_residual`]):
    /// the fillable quantity is what matters, failures are left to the
    /// sweep itself.
    ///
    /// Deliberately relaxes `PriceLevel::match_requirements`' precondition
    /// (every mutator of the level excluded until the last match): only
    /// `MatchRequirements::fillable` is read, which is the same dry run as
    /// `PriceLevel::matchable_quantity` and just as advisory when another
    /// submit can run under the shared gate. No counter, trade-id or
    /// reservation decision is taken from it.
    DepthOnly,
}

/// What admission validation (`validate_order_shape`) measured for the sweep
/// that follows it: the fill-or-kill preflight (#240) and the highest price
/// the trade arithmetic preflight verified (#244), which seeds the sweep's
/// per-level backstop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ShapeVerdict {
    /// Fill-or-kill feasibility, `None` for every other time-in-force.
    pub(crate) fok: Option<FokFeasibility>,
    /// Every price at or below this one prices the taker's quantity
    /// exactly; `0` when the taker cannot trade (or is post-only).
    pub(crate) arithmetic_verified_price: u128,
}

/// Overflow of a fill-or-kill feasibility accumulator. Every accumulator is
/// bounded by the taker's `u64` quantity, so this is an invariant breach,
/// reported as a typed error instead of clamping.
#[cold]
#[inline(never)]
fn fok_counter_overflow() -> OrderBookError {
    OrderBookError::InvalidOperation {
        message: "fill-or-kill feasibility accumulator overflowed u64".to_string(),
    }
}

/// A level the fill-or-kill preflight found poisoned (PriceLevel#217): it
/// refuses every match, so the taker is rejected before anything mutates.
/// Same variant pricelevel reports for a poisoned level.
#[cold]
#[inline(never)]
fn poisoned_level_error(price: u128) -> PriceLevelError {
    PriceLevelError::InvalidOperation {
        message: format!(
            "price level {price} is poisoned and refuses every match; reconstruct the book from a snapshot"
        ),
    }
}

/// `peek_match` accounting failed: the matched quantity exceeded the
/// requested one. Each level adds at most what is still needed, so this is
/// an invariant breach, reported instead of clamped (#246).
#[cold]
#[inline(never)]
fn peek_accounting_breach() -> OrderBookError {
    OrderBookError::InvalidOperation {
        message: "peek_match accounting exceeded the requested quantity".to_string(),
    }
}

/// The abort source for a level the sweep's arithmetic backstop refused
/// (#244): pricelevel's `InvalidOperation`, since `MatchAborted` carries a
/// `PriceLevelError`. Cold: only reached when a maker admitted concurrently
/// under the shared gate outruns the preflight.
#[cold]
#[inline(never)]
fn arithmetic_abort_source(price: u128, err: &OrderBookError) -> PriceLevelError {
    PriceLevelError::InvalidOperation {
        message: format!(
            "level {price} not matched: worst-case notional or fee is not representable ({err})"
        ),
    }
}

/// A sweep's scratch buffers: `(filled_orders, empty_price_levels,
/// strandable_makers, stp_orders)`.
type SweepBuffers = (
    Vec<(Id, u64)>,
    Vec<u128>,
    Option<Vec<(Id, u64)>>,
    Vec<std::sync::Arc<OrderType<()>>>,
);

/// Take a sweep's scratch buffers from the thread-local pool.
///
/// `strandable_makers` is only drawn when `watch_strandable` is set and
/// `stp_orders` only when `stp_active`; otherwise they are `None` / an empty
/// `Vec::new()` that allocates nothing.
///
/// The pool is a cache, never state (#246): `LocalKey::try_with` fails once
/// the thread-local has been destroyed (a sweep run from another
/// thread-local's destructor during thread teardown), and the pool itself
/// hands out a fresh buffer when it is already borrowed. Both degrade to
/// fresh, empty buffers instead of panicking; only the reuse is lost.
#[inline]
fn acquire_sweep_buffers(watch_strandable: bool, stp_active: bool) -> SweepBuffers {
    MATCHING_POOL
        .try_with(|pool| {
            (
                pool.get_filled_orders_vec(),
                pool.get_price_vec(),
                watch_strandable.then(|| pool.get_filled_orders_vec()),
                if stp_active {
                    pool.get_order_snapshot_vec()
                } else {
                    Vec::new()
                },
            )
        })
        .unwrap_or_else(|_destroyed| fresh_sweep_buffers(watch_strandable))
}

/// Fresh scratch buffers for a sweep that cannot reach its thread-local
/// pool (#246). Cold: only thread teardown gets here.
#[cold]
#[inline(never)]
fn fresh_sweep_buffers(watch_strandable: bool) -> SweepBuffers {
    (
        Vec::new(),
        Vec::new(),
        watch_strandable.then(Vec::new),
        Vec::new(),
    )
}

/// Return a sweep's scratch buffers to the thread-local pool. `stp_orders`
/// only came from the pool when STP was active; otherwise it is an empty,
/// never-filled `Vec` that is simply dropped. When the pool has already been
/// destroyed (thread teardown, #246) the closure never runs and the buffers
/// are dropped with it.
#[inline]
fn release_sweep_buffers(
    filled_orders: Vec<(Id, u64)>,
    empty_price_levels: Vec<u128>,
    strandable_makers: Option<Vec<(Id, u64)>>,
    stp_orders: Vec<std::sync::Arc<OrderType<()>>>,
    stp_active: bool,
) {
    // `Err` only means the pool is gone; the buffers were dropped with the
    // closure, which is all a release has to achieve.
    let _pool_destroyed = MATCHING_POOL.try_with(|pool| {
        pool.return_filled_orders_vec(filled_orders);
        if let Some(strandable) = strandable_makers {
            pool.return_filled_orders_vec(strandable);
        }
        pool.return_price_vec(empty_price_levels);
        if stp_active {
            pool.return_order_snapshot_vec(stp_orders);
        }
    });
}

/// A reservation count as `usize`.
///
/// # Errors
///
/// [`PriceLevelError::CapacityExceeded`] when it does not fit.
#[inline]
fn steps_to_usize(steps: u64) -> Result<usize, PriceLevelError> {
    usize::try_from(steps).map_err(|_| PriceLevelError::CapacityExceeded {
        resource: CapacityResource::Trades,
        additional: usize::MAX,
    })
}

/// Reserve `additional` more slots in the sweep's pooled filled-maker
/// buffer, fallibly.
///
/// # Errors
///
/// [`PriceLevelError::CapacityExceeded`] when it cannot grow.
#[inline]
fn reserve_filled_makers(
    filled_orders: &mut Vec<(Id, u64)>,
    additional: usize,
) -> Result<(), PriceLevelError> {
    filled_orders
        .try_reserve(additional)
        .map_err(|_| PriceLevelError::CapacityExceeded {
            resource: CapacityResource::FilledOrderIds,
            additional,
        })
}

/// The per-level budget arithmetic of a sweep failed (#246): a level
/// reported more remaining quantity than it was asked to fill, or the
/// executed quantity exceeded the budget it was capped by. Both are ruled
/// out by pricelevel's `MatchResult` invariant and by the level cap, so this
/// is an invariant breach, reported as the level's failure (the sweep stops
/// there with its committed prefix) instead of being clamped.
#[cold]
#[inline(never)]
fn sweep_budget_breach(what: &'static str, quantity: u64, price: u128) -> PriceLevelError {
    PriceLevelError::InvalidOperation {
        message: format!(
            "sweep budget accounting failed ({what}): quantity {quantity} at price {price}"
        ),
    }
}

/// Quantity a level executed for a request of `requested` units: the
/// request minus what the level left unfilled (#246: checked).
///
/// # Errors
///
/// [`PriceLevelError::InvalidOperation`] when the level reports more
/// remaining quantity than it was asked for (an invariant breach).
#[inline]
fn level_executed(
    requested: u64,
    level_match: &MatchResult,
    price: u128,
) -> Result<u64, PriceLevelError> {
    requested
        .checked_sub(level_match.remaining_quantity().as_u64())
        .ok_or_else(|| sweep_budget_breach("level remaining exceeds request", requested, price))
}

/// The first maker `level_match` filled that is absent from the STP
/// `snapshot` the level's verdict was taken on (#225).
///
/// `None` is the invariant: an STP-engaged sweep holds the exclusive submit
/// gate, so nothing can join the level between the scan and the match.
/// `O(trades × snapshot)`, so it only runs in debug builds (see
/// [`report_stp_snapshot_breach`]).
#[must_use]
fn stp_snapshot_breach(
    snapshot: &[std::sync::Arc<OrderType<()>>],
    level_match: &MatchResult,
) -> Option<Id> {
    level_match
        .trades()
        .as_vec()
        .iter()
        .map(|trade| trade.maker_order_id())
        .find(|maker| !snapshot.iter().any(|order| order.id() == *maker))
}

/// Report a breach of the #225 STP snapshot invariant found by
/// [`stp_snapshot_breach`] (#246: replaces three `debug_assert!`s).
///
/// Runs in debug builds only, like the assertions it replaces, but logs at
/// `ERROR` instead of panicking: the trade is already committed, so there is
/// nothing to roll back and a panic would only lose the sweep's bookkeeping.
#[cold]
#[inline(never)]
fn report_stp_snapshot_breach(site: &'static str, price: u128, maker: Id) {
    tracing::error!(
        site,
        price,
        maker_order_id = %maker,
        "#225 invariant breached: the sweep filled a maker absent from the STP snapshot (exclusive submit gate not held?)"
    );
}

/// Add the depth a single resting order contributes to a sweep to `acc`,
/// bounded by `cap`: `min(cap, acc + visible + drawable hidden)` (#246).
///
/// The drawable hidden quantity is the part the sweep can actually draw: an
/// iceberg always replenishes its hidden tranche; a reserve only when
/// `auto_replenish` is set — a non-auto-replenish reserve drops its hidden
/// unfilled, so that hidden is NOT reachable depth.
///
/// As of #136 the non-STP and STP-NoConflict FOK feasibility paths delegate to
/// `PriceLevel::matchable_quantity` (the authoritative upstream dry run). This
/// helper remains only for the STP `CancelMaker` case, which must sum the
/// matchable depth of the *non-self* makers — a per-user filter the upstream
/// primitive cannot express. It is therefore the one FOK-feasibility path that
/// no longer rides on pricelevel's authoritative dry run, so its per-order total
/// MUST stay equal to `OrderType::match_against`'s for every resting kind: if a
/// new `OrderType` variant is added whose sweep total differs from
/// `visible + drawable_hidden`, update this helper or the CancelMaker FOK path
/// will silently mis-predict while the delegated paths stay correct.
///
/// Precondition `acc <= cap`, kept by [`capped_depth_add`]; the result is
/// then the exact `min(cap, depth)` even when the depth overflows `u64` (see
/// that function).
#[inline]
#[must_use]
fn add_order_matchable_qty(acc: u64, order: &OrderType<()>, cap: u64) -> u64 {
    let visible = order.visible_quantity().as_u64();
    let drawable_hidden = match order {
        OrderType::IcebergOrder {
            hidden_quantity, ..
        } => hidden_quantity.as_u64(),
        OrderType::ReserveOrder {
            hidden_quantity,
            auto_replenish,
            ..
        } if *auto_replenish => hidden_quantity.as_u64(),
        _ => 0,
    };
    capped_depth_add(capped_depth_add(acc, visible, cap), drawable_hidden, cap)
}

/// Hidden quantity `filled_id` would strand, looked up in the sweep's
/// captured strandable makers (#230).
///
/// `strandable` is sorted by [`Id::as_bytes`] — `Id` does not implement
/// `Ord` upstream, so that stable 16-byte projection is the sort key. Two
/// *different* ids can share one projection (`Sequential` zero-pads into the
/// same 16 bytes a `Uuid` could occupy), so the equal-key run is walked and
/// the ids compared for real equality rather than trusting the key alone.
/// That run is length 1 in every realistic book.
#[must_use]
#[inline]
fn find_strandable(strandable: &[(Id, u64)], filled_id: Id) -> Option<u64> {
    let key = filled_id.as_bytes();
    let start = strandable.partition_point(|(id, _)| id.as_bytes() < key);
    strandable
        .get(start..)?
        .iter()
        .take_while(|(id, _)| id.as_bytes() == key)
        .find(|(id, _)| *id == filled_id)
        .map(|(_, hidden)| *hidden)
}

/// Releases the book indices of the makers a sweep filled, in
/// `filled_orders` order, including when the drain unwinds (#294).
///
/// The drain runs caller code per maker before releasing that maker's
/// location: `track_state` reads the tracker's `Clock` and may call the
/// metrics recorder, and the strandable-maker report calls `tracing` and
/// the metrics recorder. The location is the id's ownership token (#288)
/// and must be released **after** the maker's `Filled` state is recorded,
/// otherwise a same-id order admitted in between would have its resting
/// state overwritten by the old order's `Filled`; so the release cannot
/// simply move ahead of the caller code. Instead, [`Self::release_next`]
/// releases one maker after its caller code ran, and the guard's `Drop`
/// releases every maker not released yet when that caller code unwinds.
/// Either way no filled maker keeps a ghost location, user-index entry or
/// strandable count. The drop does crate-owned index work only (no
/// allocation, no caller code apart from the count's `WARN` on a refused
/// decrement), and the unwind itself engages the kill switch through the
/// submit gate guard.
struct FilledMakerRelease<'a, T: Clone + Send + Sync + Default + 'static> {
    /// The sweeping book.
    book: &'a OrderBook<T>,
    /// Every maker the sweep filled, `(id, filled quantity)`.
    filled: &'a [(Id, u64)],
    /// The sweep's captured strandable makers, sorted by id bytes.
    strandable: Option<&'a [(Id, u64)]>,
    /// Index of the first maker not released yet.
    next: usize,
}

impl<T: Clone + Send + Sync + Default + 'static> FilledMakerRelease<'_, T> {
    /// Release the indices of the maker at `self.next` and advance.
    /// `strandable` is whether the caller already found it in the capture
    /// list (saves a second lookup).
    #[inline]
    fn release_next(&mut self, strandable: bool) {
        if let Some((filled_id, _)) = self.filled.get(self.next) {
            release_filled_maker(self.book, *filled_id, strandable);
        }
        // `next < filled.len() <= isize::MAX`: the fallback is unreachable
        // and would mean "everything released".
        self.next = self.next.checked_add(1).unwrap_or(self.filled.len());
    }
}

impl<T: Clone + Send + Sync + Default + 'static> Drop for FilledMakerRelease<'_, T> {
    fn drop(&mut self) {
        // Non-empty only when the drain unwound mid-maker.
        for (filled_id, _) in self.filled.get(self.next..).unwrap_or_default() {
            let strandable = self
                .strandable
                .is_some_and(|strandable| find_strandable(strandable, *filled_id).is_some());
            release_filled_maker(self.book, *filled_id, strandable);
        }
    }
}

/// Release one filled maker's indices: the strandable-maker count (#230),
/// the user index, then the location (#288: untrack before releasing the
/// id, the location is its ownership token).
#[inline]
fn release_filled_maker<T: Clone + Send + Sync + Default + 'static>(
    book: &OrderBook<T>,
    filled_id: Id,
    strandable: bool,
) {
    if strandable {
        // #230: the fill drain is the third and last place a strandable
        // maker leaves a level. Being in the capture list AND in
        // `filled_orders` is exactly that — and, because a sweep in such a
        // book runs exclusively, the two really are the same order rather
        // than an id reused in between.
        book.note_removed_strandable_maker();
    }
    book.untrack_located_order(&filled_id);
    book.order_locations.remove(&filled_id);
}

/// Outcome of an internal match: the [`MatchResult`] plus whether self-trade
/// prevention cancelled the taker. The flag lets the resting caller (`add_order`)
/// know it must NOT rest the residual — a partially-filled taker that then
/// self-crossed under `CancelTaker` / `CancelBoth` is cancelled, not rested (#97).
#[derive(Debug)]
pub(crate) struct MatchOutcome {
    /// The trades, filled-order ids, and remaining quantity of the match.
    pub(crate) result: MatchResult,
    /// `true` when STP cancelled the taker, so any residual must not rest.
    pub(crate) taker_stp_cancelled: bool,
    /// `true` when a per-level post-only guard refused to trade (#209):
    /// the taker was submitted as [`TakerKind::PostOnly`] and reached a
    /// crossable level, so the book must reject it (`PriceCrossing`) —
    /// structurally zero trades were emitted.
    pub(crate) taker_post_only_rejected: bool,
    /// `Some(OrderBookError::MatchAborted)` when the sweep stopped at a
    /// failed price level (#240). `result` then holds exactly the committed
    /// prefix, which the caller must publish like a partial fill before it
    /// returns this error; the remainder must never rest. The taker's
    /// terminal `Cancelled { MatchAborted }` state and the reject metric are
    /// already recorded by the sweep.
    pub(crate) aborted: Option<OrderBookError>,
}

impl MatchOutcome {
    /// Wrap a `MatchResult` with no STP cancellation (common case / empty book).
    #[inline]
    fn resting(result: MatchResult) -> Self {
        Self {
            result,
            taker_stp_cancelled: false,
            taker_post_only_rejected: false,
            aborted: None,
        }
    }

    /// The bare `MatchResult`, or the abort error when the sweep stopped at
    /// a failed level. Used by the raw `match_*` entry points, which hand
    /// the result to the caller instead of publishing it.
    ///
    /// # Errors
    ///
    /// [`OrderBookError::MatchAborted`] when [`Self::aborted`] is set.
    #[inline]
    pub(crate) fn into_result(self) -> Result<MatchResult, OrderBookError> {
        match self.aborted {
            Some(err) => Err(err),
            None => Ok(self.result),
        }
    }
}

/// Selects how the matching loop measures its budget.
///
/// `BaseQty` is the legacy base-asset quantity path (existing market and
/// limit orders). `QuoteAmount` is the quote-notional path used by
/// `match_market_order_by_amount` (Binance `quoteOrderQty` semantics).
/// Always `None` limit price for the notional path — quote-notional is
/// market-only.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MatchMode {
    /// Base-quantity match. `limit_price = None` for market orders.
    BaseQty {
        /// Total base-asset quantity to match.
        quantity: u64,
        /// Optional price ceiling (Buy) / floor (Sell). `None` for
        /// market orders.
        limit_price: Option<u128>,
    },
    /// Quote-notional match (market-only).
    QuoteAmount {
        /// Total quote-asset value to consume from the book.
        amount: u128,
    },
}

impl MatchMode {
    /// Returns the limit-price guard used inside the level walk. `None`
    /// for any market path (base-qty market or quote-notional).
    #[inline]
    #[must_use]
    fn limit_price(&self) -> Option<u128> {
        match self {
            Self::BaseQty { limit_price, .. } => *limit_price,
            Self::QuoteAmount { .. } => None,
        }
    }

    /// Returns the initial `MatchResult` quantity slot. For quote-notional
    /// the actual base-qty filled is unknown upfront, so `u64::MAX` is
    /// used as a working upper bound during the loop (see
    /// `MatchResult::add_trade` invariants). The notional path is
    /// normalized at the end of `match_order_inner` so that the returned
    /// `MatchResult.remaining_quantity()` is `0` rather than the sentinel.
    #[inline]
    #[must_use]
    fn initial_match_quantity(&self) -> u64 {
        match self {
            Self::BaseQty { quantity, .. } => *quantity,
            Self::QuoteAmount { .. } => u64::MAX,
        }
    }
}

/// Tracks the matching loop's remaining budget against either base
/// quantity or quote notional. Designed to keep the base-qty hot path
/// allocation- and branch-light: the `BaseQty` arm of every helper is
/// a single arithmetic op the optimizer can fold.
#[derive(Debug, Clone)]
enum StopCondition {
    /// Base-quantity remaining.
    BaseQty {
        /// Base-asset quantity left to fill.
        remaining: u64,
    },
    /// Quote-notional remaining.
    QuoteAmount {
        /// Quote-asset value left to consume.
        remaining: u128,
    },
}

impl StopCondition {
    /// Build a fresh stop condition from the matching mode.
    #[inline]
    fn from_mode(mode: &MatchMode) -> Self {
        match mode {
            MatchMode::BaseQty { quantity, .. } => Self::BaseQty {
                remaining: *quantity,
            },
            MatchMode::QuoteAmount { amount } => Self::QuoteAmount { remaining: *amount },
        }
    }

    /// Per-level base-qty cap respecting `lot_size`. A return of `0`
    /// signals the caller to stop walking (dust below one full lot at
    /// the current level price).
    ///
    /// `lot <= 1` ⇒ no rounding (single arithmetic path); preserves the
    /// existing base-qty performance profile when lot enforcement is not
    /// configured.
    #[inline]
    #[must_use]
    fn level_qty_cap(&self, level_price: u128, lot: u64) -> u64 {
        let raw = match self {
            Self::BaseQty { remaining } => *remaining,
            Self::QuoteAmount { remaining } => {
                // Whole units the budget funds at this price; a zero price
                // funds nothing (and is never divided by).
                let Some(units) = remaining.checked_div(level_price) else {
                    return 0;
                };
                // A level match takes a `u64` request, and the taker's
                // `MatchResult` cannot record more than `u64::MAX` units in
                // total, so a budget funding more than that at this price
                // requests `u64::MAX`: the most any sweep can execute. What
                // the level does not fill stays in the notional budget
                // (`consume` deducts only the executed quantity).
                u64::try_from(units).unwrap_or(u64::MAX)
            }
        };
        if lot <= 1 {
            raw
        } else {
            // Round down to a whole lot. `lot > 1` and `raw % lot <= raw`,
            // so neither step can fail; a failure would stop the walk (zero
            // cap), never over-fill.
            raw.checked_rem(lot)
                .and_then(|dust| raw.checked_sub(dust))
                .unwrap_or(0)
        }
    }

    /// Decrement the remaining budget by what was actually executed at
    /// the given price.
    ///
    /// The executed quantity never exceeds the level cap derived from this
    /// budget (`level_qty_cap`), so neither the subtraction nor the notional
    /// product can fail; both are checked anyway (#246).
    ///
    /// # Errors
    ///
    /// [`PriceLevelError::InvalidOperation`] when `executed_qty` exceeds the
    /// remaining budget (an invariant breach). The budget is left unchanged.
    #[inline]
    fn consume(&mut self, executed_qty: u64, level_price: u128) -> Result<(), PriceLevelError> {
        let next = match self {
            Self::BaseQty { remaining } => remaining.checked_sub(executed_qty).map(|left| {
                *remaining = left;
            }),
            Self::QuoteAmount { remaining } => level_price
                .checked_mul(u128::from(executed_qty))
                .and_then(|spent| remaining.checked_sub(spent))
                .map(|left| {
                    *remaining = left;
                }),
        };
        next.ok_or_else(|| {
            sweep_budget_breach("executed exceeds budget", executed_qty, level_price)
        })
    }

    /// Returns `true` when no further fills are needed (budget exhausted).
    #[inline]
    #[must_use]
    fn is_done(&self) -> bool {
        match self {
            Self::BaseQty { remaining } => *remaining == 0,
            Self::QuoteAmount { remaining } => *remaining == 0,
        }
    }

    /// Whether the remaining budget is quote-notional dust at `level_price`:
    /// nonzero, but unable to fund one more lot at that price.
    ///
    /// Only the notional arm is walked past on the STP-cancelling paths. A
    /// base-quantity residual, whatever its size, is the taker's own
    /// quantity still unfilled at a level holding its own maker; walked
    /// past, it would rest crossed against that maker (a maker admitted
    /// before a lot-size change keeps resting with a misaligned tranche —
    /// see `set_lot_size` — so a sub-lot residual is reachable), so the
    /// base arm keeps the STP verdict. A
    /// notional taker never rests, and its dust at one price can still
    /// fund a whole lot at a cheaper level, so it walks on instead.
    #[inline]
    #[must_use]
    fn is_dust_at(&self, level_price: u128, lot: u64) -> bool {
        matches!(self, Self::QuoteAmount { .. }) && self.level_qty_cap(level_price, lot) == 0
    }

    /// Whether a zero [`Self::level_qty_cap`] at the level the walk is
    /// standing on ends the whole walk, or only this level.
    ///
    /// The taker's side fixes the walk direction and therefore the price
    /// monotonicity of the levels still ahead: a buy taker walks asks
    /// ascending, so every later level is **dearer**; a sell taker walks
    /// bids descending, so every later level is **cheaper**.
    ///
    /// - `BaseQty`: the cap is the lot-rounded residual and does not depend
    ///   on the level price at all. Zero here is zero at every remaining
    ///   level whichever way the walk runs, so it is terminal on both sides.
    /// - `QuoteAmount` on a buy: the cap is `remaining / level_price`, which
    ///   is non-increasing as the walk moves to dearer asks. A budget that
    ///   cannot fund one lot here funds even less further on, so zero is
    ///   terminal.
    /// - `QuoteAmount` on a sell: the walk moves to cheaper bids, so
    ///   `remaining / level_price` can rise again. A budget that cannot fund
    ///   one lot at this bid may fund a whole one at a lower bid, so the
    ///   level is skipped and the walk continues — unless the budget cannot
    ///   fund a lot at **any** price, which is the exact test below.
    ///
    /// The sell arm's exact terminal condition is `remaining < lot`. The cap
    /// is `(remaining / level_price)` rounded down to a multiple of `lot`,
    /// and the cheapest a level can be is a price of `1`, at which the cap is
    /// `remaining` itself; no level can yield more. So `remaining < lot`
    /// means every level still ahead caps at zero and the walk is over,
    /// while `remaining >= lot` means some reachable price would fund a lot
    /// and the walk must go on to find out whether the book offers one.
    /// With no lot size configured (`lot <= 1`) that reduces to
    /// `remaining == 0`, which the loop's own `is_done` check already
    /// handles, so a notional sell then stops only on an exhausted budget or
    /// an exhausted side.
    ///
    /// Traversal cost: a notional sell whose budget stays at or above one
    /// lot visits every remaining level on its side, which is strictly more
    /// than the old unconditional break did. The bound is the number of
    /// levels resting on that side; each skipped level costs one `u128`
    /// divide and no level mutation.
    ///
    /// Only ever consulted when the cap is already zero, so the base-qty
    /// hot path (`lot <= 1`, cap equal to the residual) never evaluates it.
    #[inline]
    #[must_use]
    fn zero_cap_is_terminal(&self, taker_side: Side, lot: u64) -> bool {
        match self {
            Self::BaseQty { .. } => true,
            Self::QuoteAmount { remaining } => {
                matches!(taker_side, Side::Buy) || *remaining < u128::from(lot.max(1))
            }
        }
    }
}

impl<T> OrderBook<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Highly optimized internal matching function.
    ///
    /// This is the backward-compatible entry point that delegates to
    /// [`Self::match_order_with_user`] with `Hash32::zero()` (bypasses STP).
    ///
    /// # Performance Optimization
    /// Uses SkipMap which maintains prices in sorted order automatically.
    /// This eliminates O(N log N) sorting overhead, reducing time complexity
    /// from O(N log N) to O(M log N), where:
    /// - N = total number of price levels
    /// - M = number of price levels actually matched (typically << N)
    ///
    /// In the happy case (single price level fill), complexity is O(log N).
    ///
    /// # Concurrency
    ///
    /// An anonymous sweep takes the shared side of the submit gate (unless
    /// a strandable maker rests), so two concurrent calls can match at the
    /// same price level at once. Their trades and the level's queue stay
    /// exact; the level's execution statistics seen by a concurrent snapshot
    /// are advisory until both return (see [`OrderBook`]'s "Level statistics
    /// are advisory under concurrent takers").
    ///
    /// # Errors
    ///
    /// Same as [`Self::match_order_with_user`]. Like every raw `match_*`
    /// entry point, an aborted partial fill (#240) returns
    /// [`OrderBookError::MatchAborted`] carrying only a summary
    /// (`executed_quantity`, `trade_count`): the raw family publishes no
    /// trades, so the committed prefix is lost to the caller. Install a
    /// trade listener and use a publishing entry point, or the submit
    /// `*_with_committed` APIs, to see it.
    pub fn match_order(
        &self,
        order_id: Id,
        side: Side,
        quantity: u64,
        limit_price: Option<u128>,
    ) -> Result<MatchResult, OrderBookError> {
        // #209: shared submit gate; calls the ungated outcome variant so
        // the non-reentrant gate is acquired exactly once.
        //
        // #230: through the coherent helper, so this sweep upgrades to the
        // exclusive side in a book that holds strandable makers. An
        // anonymous taker never runs the STP scan, so the strandable rule is
        // the only one that can apply here — but it must apply: on the
        // shared side a concurrent cancel could remove a maker this sweep
        // has already captured and free its id for an unrelated order, and
        // the drain would then report a discard that never happened.
        let _gate = self.acquire_coherent_submit_gate(false);
        let result = (|| {
            // #244: worst-case notional / fee representability, before any
            // mutation — identical to every publishing entry point.
            let verified =
                self.check_trade_arithmetic_or_reject(order_id, side, quantity, limit_price)?;
            self.match_order_with_user_outcome(
                order_id,
                side,
                quantity,
                limit_price,
                Hash32::zero(),
                TakerKind::Standard,
                SweepReservation::NONE,
                verified,
            )
            .and_then(MatchOutcome::into_result)
        })();
        // #286: under the same gate, after the taker's trades.
        self.settle_pending_stops();
        result
    }

    /// Internal matching function with Self-Trade Prevention support.
    ///
    /// When `taker_user_id` is `Hash32::zero()` or `stp_mode` is `None`,
    /// the STP check is skipped entirely (zero overhead fast path).
    ///
    /// # Arguments
    /// * `order_id` — The taker (incoming) order's unique identifier.
    /// * `side` — The side of the incoming order (`Buy` or `Sell`).
    /// * `quantity` — The quantity to match.
    /// * `limit_price` — Optional price limit (`None` for market orders).
    /// * `taker_user_id` — The user ID of the incoming order for STP checks.
    ///
    /// # Errors
    /// Returns [`OrderBookError::InsufficientLiquidity`] for market orders
    /// when no liquidity is available, or [`OrderBookError::SelfTradePrevented`]
    /// when STP in `CancelTaker` or `CancelBoth` mode cancels the entire taker.
    /// Returns [`OrderBookError::MatchAborted`] when the sweep stopped at a
    /// price level that reported a failure (#240): the trades committed
    /// before it are real and stay executed (makers, risk, order state and
    /// price-level listener reflect them), but this raw entry point does not
    /// publish trades, so the error's `executed_quantity` / `trade_count`
    /// are all it reports. Use a publishing entry point (`add_order`,
    /// `submit_*`, `match_*_order`) to receive the committed trades.
    pub fn match_order_with_user(
        &self,
        order_id: Id,
        side: Side,
        quantity: u64,
        limit_price: Option<u128>,
        taker_user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        // #209: submit gate (see `match_order`). #225: exclusive when STP
        // is engaged for this taker, so the per-level scan and the fill it
        // authorises see the same queue state. Match-only entry points
        // always sweep as `TakerKind::Standard`, hence `is_post_only =
        // false`.
        let _gate = self.acquire_coherent_submit_gate(self.submit_needs_exclusive_gate(
            false,
            taker_user_id,
            false,
            false,
        ));
        let result = (|| {
            // #244: see `match_order`.
            let verified =
                self.check_trade_arithmetic_or_reject(order_id, side, quantity, limit_price)?;
            self.match_order_with_user_outcome(
                order_id,
                side,
                quantity,
                limit_price,
                taker_user_id,
                TakerKind::Standard,
                SweepReservation::NONE,
                verified,
            )
            .and_then(MatchOutcome::into_result)
        })();
        // #286: under the same gate, after the taker's trades.
        self.settle_pending_stops();
        result
    }

    /// Like [`Self::match_order_with_user`] but returns the full [`MatchOutcome`],
    /// including the STP-cancel signal the resting caller in `add_order` needs to
    /// avoid resting a self-cross residual (#97) and the abort (#240).
    ///
    /// `reservation` is the fill-or-kill preflight reservation
    /// ([`FokFeasibility::reservation`]); [`SweepReservation::NONE`] for
    /// every other taker.
    /// `arithmetic_verified_price` is what the trade arithmetic preflight
    /// returned (#244); the sweep's backstop re-checks only levels above it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn match_order_with_user_outcome(
        &self,
        order_id: Id,
        side: Side,
        quantity: u64,
        limit_price: Option<u128>,
        taker_user_id: Hash32,
        taker_kind: TakerKind,
        reservation: SweepReservation,
        arithmetic_verified_price: u128,
    ) -> Result<MatchOutcome, OrderBookError> {
        self.match_order_inner(
            order_id,
            side,
            MatchMode::BaseQty {
                quantity,
                limit_price,
            },
            taker_user_id,
            taker_kind,
            reservation,
            arithmetic_verified_price,
        )
    }

    /// Internal entry point for the quote-notional matching path.
    ///
    /// Public callers reach this through
    /// [`OrderBook::match_market_order_by_amount_with_user`]; the function
    /// here is the matching-loop seam that drives the unified inner loop
    /// with `MatchMode::QuoteAmount`. Always market-only — there is no
    /// `limit_price` analogue for notional orders.
    ///
    /// # Errors
    /// Returns [`OrderBookError::InsufficientLiquidityNotional`] when no
    /// liquidity could be consumed (empty book or budget below one full
    /// lot at every reachable level), or
    /// [`OrderBookError::SelfTradePrevented`] when STP cancels the taker
    /// before any fills occur. A sweep stopped by a failed price level
    /// comes back as `Ok` with [`MatchOutcome::aborted`] set; the caller
    /// publishes the committed prefix and returns the abort (#240).
    pub(crate) fn match_order_by_amount_with_user(
        &self,
        order_id: Id,
        side: Side,
        amount: u128,
        taker_user_id: Hash32,
    ) -> Result<MatchOutcome, OrderBookError> {
        self.match_order_inner(
            order_id,
            side,
            MatchMode::QuoteAmount { amount },
            taker_user_id,
            TakerKind::Standard,
            SweepReservation::NONE,
            // Unused: a quote-notional sweep is bounded by its amount.
            0,
        )
    }

    /// Unified matching loop driven by [`MatchMode`] / [`StopCondition`].
    ///
    /// One inner implementation handles both base-quantity and
    /// quote-notional walks. The `BaseQty` path is identical in shape to
    /// the previous implementation: `level_qty_cap` is a no-op for
    /// `lot <= 1` and a single checked `% lot` otherwise; `consume` is one
    /// `checked_sub` per level. The `QuoteAmount` path adds one
    /// `u128` divide per level (to derive the per-level qty cap) and one
    /// `u128` multiply per fill (to deduct from the remaining notional).
    ///
    /// `lot_size`, when configured, is enforced uniformly: per-level qty
    /// is rounded **down** to a multiple of `lot`. This complements the
    /// admission-time validation in `modifications.rs` and ensures
    /// notional walks never emit `qty=0` trades when budget is below one
    /// full lot.
    ///
    /// # Aborted sweeps (#240)
    ///
    /// A level whose `MatchResult::error()` is set committed a prefix and
    /// then failed; folding a level's trades into the aggregate can fail as
    /// well. Either way the sweep stops at that level — no later (worse)
    /// level is touched — the post-sweep bookkeeping still runs for every
    /// maker actually consumed, the taker is recorded as
    /// `Cancelled { MatchAborted }` with the committed quantity, and the
    /// outcome comes back with [`MatchOutcome::aborted`] set and `result`
    /// holding exactly the committed prefix.
    ///
    /// A non-empty `reservation` (fill-or-kill only) reserves the trade and
    /// filled-id buffers before the first level is touched; a refused
    /// reservation rejects the taker untouched with
    /// [`OrderBookError::PriceLevelError`] (`CapacityExceeded`).
    ///
    /// # Folding levels (#240, PriceLevel#219)
    ///
    /// Each level's result is folded into the aggregate with
    /// `MatchResult::try_absorb` when the level was asked for exactly the
    /// aggregate's remaining quantity (every base-quantity level outside a
    /// self-trade prevention pre-match whose remaining quantity is a lot
    /// multiple, which admission guarantees for submitted orders), and trade
    /// by trade otherwise (quote-notional levels, STP pre-matches, a
    /// lot-rounded level cap). Nothing can fail once a
    /// level mutated, because the room the fold needs is reserved before the
    /// level is touched (`reserve_level_fold`), except while the aggregate
    /// is still empty on the absorb path: absorbing into an empty aggregate
    /// adopts the level's own buffers, which cannot fail and allocates
    /// nothing. A refused fold leaves the aggregate unchanged (all or
    /// nothing, as before), so the published prefix is always a whole number
    /// of levels.
    #[allow(clippy::too_many_arguments)]
    fn match_order_inner(
        &self,
        order_id: Id,
        side: Side,
        mode: MatchMode,
        taker_user_id: Hash32,
        taker_kind: TakerKind,
        reservation: SweepReservation,
        arithmetic_verified_price: u128,
    ) -> Result<MatchOutcome, OrderBookError> {
        let mut match_result =
            MatchResult::new(order_id, Quantity::new(mode.initial_match_quantity()));
        let mut stop = StopCondition::from_mode(&mode);
        let limit_price = mode.limit_price();
        let lot = self.lot_size.unwrap_or(1);
        // Deterministic taker timestamp for per-level matching: `pricelevel` 0.8's
        // `match_order` no longer reads the wall clock. Computed once so every trade
        // in this submit shares the taker's match time and replay stays deterministic.
        let taker_ts = self.clock().now_millis();

        // Determine if STP checks are needed for this match
        let stp_active = self.stp_mode.is_enabled() && taker_user_id != Hash32::zero();

        // Choose the appropriate side for matching
        let match_side = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        // Early exit if the opposite side is empty
        if match_side.is_empty() {
            return self
                .empty_book_result(side, &mode, match_result)
                .map(MatchOutcome::resting);
        }

        // Get reusable vectors from pool. `filled_orders` / `empty_price_levels`
        // are needed by every sweep. `stp_orders` is the per-level STP scan
        // scratch buffer (#107), reused across conflicting levels instead of a
        // fresh allocation per level — but it is only acquired when STP is
        // active, so the default (`STPMode::None`) hot path touches the pool
        // exactly as it did before #107 (an empty `Vec::new()` allocates nothing
        // and is never filled or returned).
        //
        // #230: makers that would strand hidden depth if this sweep consumed
        // them, captured per level BEFORE it is matched (the level drops them
        // during the match, so the hidden quantity is unrecoverable after).
        // Same `(Id, quantity)` shape as `filled_orders`, so it reuses the
        // same pool.
        //
        // The gate is read ONCE here, not per level: on a book that has never
        // rested a non-auto-replenishing reserve with hidden depth the whole
        // strandable path drops out of this sweep — `None` means no pool
        // acquire, no per-level capture, no lookup in the drain loop and
        // nothing to return. That matters because the capture walks the
        // level's `DashMap` of orders, which read-locks every shard.
        let watch_strandable = self.strandable_makers_resting.load(Ordering::Relaxed) > 0;
        let (mut filled_orders, mut empty_price_levels, mut strandable_makers, mut stp_orders) =
            acquire_sweep_buffers(watch_strandable, stp_active);

        // Fill-or-kill preflight (#240): reserve the aggregate trade /
        // filled-id storage and the pooled maker buffer for everything the
        // feasibility walk predicted BEFORE the first level is touched, so
        // a refused allocation kills the taker with the book untouched
        // instead of stopping the sweep half-way. The reservation covers
        // every per-level reservation below, so none of them grows.
        if !reservation.is_none()
            && let Err(err) =
                Self::reserve_sweep(&mut match_result, &mut filled_orders, reservation)
        {
            release_sweep_buffers(
                filled_orders,
                empty_price_levels,
                strandable_makers,
                stp_orders,
                stp_active,
            );
            return Err(self.reject_untouched(order_id, err));
        }

        // Track whether STP cancelled the taker
        let mut stp_taker_cancelled = false;
        let mut post_only_rejected = false;
        // First pricelevel failure hit by the sweep (#240): a level's
        // `MatchResult::error()`, a refused fold of its committed prefix or
        // of the level's worst-case reservation, or a failed STP
        // `snapshot_by_seq_into` (both taken before the level is touched, so
        // the prefix is that of the earlier levels). The sweep stops
        // at it and never walks on to a worse level; the post-sweep
        // bookkeeping below still runs so `order_locations`, the user index
        // and the level map stay consistent with the makers already
        // consumed, and the committed prefix is surfaced as an abort.
        let mut sweep_error: Option<PriceLevelError> = None;
        // A failed post-only probe (#240): no trade and no STP action can
        // have happened, so it is a clean, untouched rejection.
        let mut post_only_probe_error: Option<PriceLevelError> = None;
        // #244 backstop: every level a base-quantity sweep trades at must
        // keep `price × quantity` and its fees representable. The preflight
        // (`check_trade_arithmetic`) verified every price up to
        // `arithmetic_verified_price`, so a level at or below it costs one
        // comparison; only a level above it (a maker admitted concurrently
        // under the shared gate, or one the preflight's visible-depth walk
        // did not expect to reach) is re-checked, and a failing level aborts
        // the sweep before it is touched. The check uses the taker's total
        // quantity, not the remaining one: `price × remaining` would not
        // bound the notional already committed at cheaper levels, while
        // `highest price × total quantity` bounds the whole sweep.
        // Quote-notional sweeps are bounded by their amount, checked by the
        // preflight.
        let arithmetic_quantity = match mode {
            MatchMode::BaseQty { quantity, .. } => Some(quantity),
            MatchMode::QuoteAmount { .. } => None,
        };
        let arithmetic_schedule = self.active_fee_schedule();
        let mut arithmetic_checked_price: u128 = arithmetic_verified_price;

        // Iterate through prices in optimal order (already sorted by SkipMap)
        // For buy orders: iterate asks in ascending order (best ask first)
        // For sell orders: iterate bids in descending order (best bid first)
        let price_iter = match side {
            Side::Buy => Either::Left(match_side.iter()),
            Side::Sell => Either::Right(match_side.iter().rev()),
        };

        // Process each price level
        for entry in price_iter {
            let price = *entry.key();
            // Check price limit constraint early (only set for limit orders)
            if let Some(limit) = limit_price {
                match side {
                    Side::Buy if price > limit => break,
                    Side::Sell if price < limit => break,
                    _ => {}
                }
            }

            // Compute per-level base-qty cap respecting both the budget
            // (base-qty or notional) and `lot_size`. A zero cap means
            // dust-below-lot at the current price: nothing can execute
            // *here*. Whether that also ends the walk depends on the
            // direction it runs in — a notional sell keeps descending to
            // cheaper bids, everything else stops. See
            // `StopCondition::zero_cap_is_terminal`.
            let qty_cap = stop.level_qty_cap(price, lot);
            if qty_cap == 0 {
                if stop.zero_cap_is_terminal(side, lot) {
                    break;
                }
                continue;
            }

            // Get price level value from the entry
            let price_level = entry.value();

            // --- Post-only crossability verdict (#209) ---
            // Resolved BEFORE the STP block: a post-only taker must be a
            // pure no-op on rejection, and the CancelMaker / CancelBoth
            // arms below cancel same-user makers as a side effect. The
            // probe delegates to pricelevel's structural guard — for a
            // `TakerKind::PostOnly` taker `match_order` is a guaranteed
            // zero-mutation dry run: `Rejected` when the level holds
            // matchable depth (self-id-skips and unmatchable resting
            // orders excluded), `NotFilled` otherwise. Post-only
            // therefore always resolves to either a clean walk-on or a
            // clean `PriceCrossing` rejection — same policy as the
            // `will_cross_market` precheck, makers untouched, STP never
            // consulted (post-only precedence over STP is intentional
            // and documented on `update_order` / `add_post_only_order`).
            if taker_kind.is_post_only() {
                let probe = price_level.match_order(
                    qty_cap,
                    order_id,
                    TimeInForce::Gtc,
                    taker_kind,
                    taker_ts,
                    &self.transaction_id_generator,
                );
                // A failed probe (pricelevel could not linearize the depth
                // scan) is marked `Rejected` too, but it carries the typed
                // error and no verdict: surface the failure, never a
                // misleading `PriceCrossing` (#240). A post-only walk never
                // trades and never reaches the STP arms, so the book is
                // provably untouched: the taker is rejected cleanly, not
                // aborted.
                if let Some(err) = probe.error() {
                    post_only_probe_error = Some(err.clone());
                    break;
                }
                if probe.outcome().was_rejected() {
                    post_only_rejected = true;
                    break;
                }
                // No matchable depth at this crossing level; walk on.
                continue;
            }

            if let Some(quantity) = arithmetic_quantity
                && price > arithmetic_checked_price
            {
                if let Err(err) = Self::check_notional_bound(arithmetic_schedule, price, quantity) {
                    sweep_error = Some(arithmetic_abort_source(price, &err));
                    break;
                }
                arithmetic_checked_price = price;
            }

            // --- STP pre-processing ---
            // When STP is active, check for self-trade conflicts before matching.
            // This is done per-price-level to handle partial fills correctly.
            let stp_action = if stp_active {
                // `check_stp_at_level` must see the resting orders in the exact order
                // the sweep consumes them — pure insertion sequence — so `safe_quantity`
                // and the CancelBoth `maker_order_id` correspond to what `match_order`
                // will actually fill/cancel. `snapshot_by_seq_into` (pricelevel 0.8.2)
                // refills the pooled `stp_orders` scratch buffer in that exact order
                // (clearing it first), so we reuse one allocation across every level
                // instead of allocating a fresh `Vec` per conflicting level (#107). The
                // order is identical to `snapshot_by_insertion_seq`: deterministic (fixing
                // the #94 DashMap non-determinism) AND faithful to the sweep even under
                // non-monotonic timestamps, unlike `snapshot_orders()` which is
                // `(timestamp, seq)`-ordered (the residual gap closed by #132 /
                // PriceLevel#102).
                // On error the buffer still holds the previous level's
                // orders (pricelevel leaves it untouched), so the STP verdict
                // must not be taken on it: stop before touching this level.
                if let Err(err) = price_level.snapshot_by_seq_into(&mut stp_orders) {
                    sweep_error = Some(err);
                    break;
                }
                Some(check_stp_at_level(
                    &stp_orders,
                    taker_user_id,
                    self.stp_mode,
                ))
            } else {
                None
            };

            // #240: reserve what folding this level can need BEFORE anything
            // touches it — including the STP arms below, whose CancelMaker /
            // CancelBoth branches cancel same-user makers — so folding its
            // committed trades cannot fail after the level was mutated. The
            // verdict above only read the level. The bound uses the
            // pre-cancel maker count, slightly conservative when STP then
            // removes makers. A refused reservation aborts the sweep here
            // with the prefix of the earlier levels, this level intact.
            let level_match_qty = match stp_action {
                Some(
                    STPAction::CancelTaker { safe_quantity }
                    | STPAction::CancelBoth { safe_quantity, .. },
                ) => qty_cap.min(safe_quantity),
                Some(STPAction::NoConflict | STPAction::CancelMaker) | None => qty_cap,
            };
            if let Err(err) = Self::reserve_level_fold(
                &mut match_result,
                &mut filled_orders,
                price_level,
                level_match_qty,
            ) {
                sweep_error = Some(err);
                break;
            }

            if let Some(action) = stp_action {
                // #225: test-only interleaving point. The verdict above was
                // taken on the queue state we just snapshotted; this is the
                // exact instant a competing mutation used to be able to slip
                // in before the level is acted on below. The hook lets a unit
                // test park here and drive that competitor deterministically.
                // Compiled out entirely outside `cfg(test)`.
                #[cfg(test)]
                if let Some(hook) = self.stp_interleave_hook.as_ref() {
                    hook(price);
                }

                match action {
                    STPAction::NoConflict => {
                        // No self-trade at this level; match normally below
                    }

                    STPAction::CancelTaker { safe_quantity } => {
                        // Match up to safe_quantity, then cancel the taker
                        if safe_quantity > 0 {
                            let match_qty = qty_cap.min(safe_quantity);
                            if match_qty > 0 {
                                if let Some(strandable) = strandable_makers.as_mut() {
                                    self.capture_strandable_makers(price_level, strandable);
                                }
                                let mut price_level_match = price_level.match_order(
                                    match_qty,
                                    order_id,
                                    TimeInForce::Gtc,
                                    taker_kind,
                                    taker_ts,
                                    &self.transaction_id_generator,
                                );
                                // #225: every maker filled here must come
                                // from the snapshot the STP verdict was taken
                                // on. A maker outside it means something
                                // landed between the scan and the sweep, i.e.
                                // the exclusive submit gate was not held.
                                if cfg!(debug_assertions)
                                    && let Some(maker) =
                                        stp_snapshot_breach(&stp_orders, &price_level_match)
                                {
                                    report_stp_snapshot_breach("cancel_taker", price, maker);
                                }
                                let executed = level_executed(match_qty, &price_level_match, price);
                                if let Err(err) = self.process_level_match(
                                    &mut match_result,
                                    &mut price_level_match,
                                    match_qty,
                                    &mut filled_orders,
                                    price,
                                    price_level,
                                    side,
                                    &mut empty_price_levels,
                                ) {
                                    sweep_error = Some(err);
                                    break;
                                }
                                if let Err(err) =
                                    executed.and_then(|executed| stop.consume(executed, price))
                                {
                                    sweep_error = Some(err);
                                    break;
                                }
                            }
                        }
                        // Reachability: the same-user maker is only reached
                        // if the taker can still execute at this price after
                        // the non-self depth in front of it. A budget the
                        // pre-match exhausted is an ordinary complete fill.
                        // Quote-notional dust — a residual that cannot fund
                        // one more unit at this price, the usual end of a
                        // notional sweep since `is_done()` is exact zero —
                        // cannot execute here either, so the maker is
                        // unreachable and survives, and the sweep walks on
                        // rather than breaking: a notional sell can still
                        // afford a whole lot at a cheaper bid (for a buy the
                        // next ask is dearer and the loop's own cap check
                        // ends the sweep). A base-quantity residual keeps
                        // the STP verdict whatever its size: walked past, it
                        // would rest crossed against the same-user maker
                        // (see `StopCondition::is_dust_at`).
                        // `check_modify_stp_self_cross` dry-runs the same
                        // decision on the modify path (#168).
                        if stop.is_done() {
                            break;
                        }
                        if stop.is_dust_at(price, lot) {
                            continue;
                        }
                        stp_taker_cancelled = true;
                        break;
                    }

                    STPAction::CancelMaker => {
                        // Cancel same-user resting orders, then match normally. We
                        // re-scan the pooled snapshot in insertion-sequence order —
                        // the exact order the old `maker_order_ids` Vec held — so the
                        // cancel order (and therefore emitted events / journal) is
                        // bit-identical to before, with no per-level allocation (#107).
                        // Each cancel runs on the level we already hold — it emits the
                        // level-change event, records OrderStatus::Cancelled
                        // { SelfTradePrevention }, and releases the per-account risk slot
                        // in lockstep, but does NOT remove the level from the map (no
                        // order_locations re-resolution either), so level removal stays
                        // with the post-walk empty_price_levels drain (#95).
                        // #247: a maker cancel the level fails stops the
                        // sweep here with the #240 abort semantics (prefix
                        // of the earlier levels published, remainder never
                        // rested) instead of carrying on as if it had been
                        // cancelled.
                        let mut maker_cancel_error = None;
                        for order in &stp_orders {
                            if order.user_id() == taker_user_id
                                && let Err(err) = self.cancel_resting_maker_on_level(
                                    price_level,
                                    side.opposite(),
                                    order.id(),
                                    CancelReason::SelfTradePrevention,
                                )
                            {
                                maker_cancel_error = Some(err);
                                break;
                            }
                        }
                        if let Some(err) = maker_cancel_error {
                            if price_level.order_count() == 0 {
                                empty_price_levels.push(price);
                            }
                            sweep_error = Some(err);
                            break;
                        }
                        // If the level is now empty, mark for removal and continue
                        if price_level.order_count() == 0 {
                            empty_price_levels.push(price);
                            continue;
                        }
                        // Fall through to normal matching below
                    }

                    STPAction::CancelBoth {
                        safe_quantity,
                        maker_order_id,
                    } => {
                        // Match up to safe_quantity, cancel the maker, then cancel taker
                        if safe_quantity > 0 {
                            let match_qty = qty_cap.min(safe_quantity);
                            if match_qty > 0 {
                                if let Some(strandable) = strandable_makers.as_mut() {
                                    self.capture_strandable_makers(price_level, strandable);
                                }
                                let mut price_level_match = price_level.match_order(
                                    match_qty,
                                    order_id,
                                    TimeInForce::Gtc,
                                    taker_kind,
                                    taker_ts,
                                    &self.transaction_id_generator,
                                );
                                // #225: see the CancelTaker arm — the makers
                                // filled here must all belong to the snapshot
                                // the STP verdict was taken on.
                                if cfg!(debug_assertions)
                                    && let Some(maker) =
                                        stp_snapshot_breach(&stp_orders, &price_level_match)
                                {
                                    report_stp_snapshot_breach("cancel_both", price, maker);
                                }
                                let executed = level_executed(match_qty, &price_level_match, price);
                                if let Err(err) = self.process_level_match(
                                    &mut match_result,
                                    &mut price_level_match,
                                    match_qty,
                                    &mut filled_orders,
                                    price,
                                    price_level,
                                    side,
                                    &mut empty_price_levels,
                                ) {
                                    sweep_error = Some(err);
                                    break;
                                }
                                if let Err(err) =
                                    executed.and_then(|executed| stop.consume(executed, price))
                                {
                                    sweep_error = Some(err);
                                    break;
                                }
                            }
                        }
                        // Same reachability rule as `CancelTaker` above, and
                        // here it also gates the maker cancellation: a maker
                        // the taker never reached must survive untouched,
                        // whether the budget is spent or only quote-notional
                        // dust is left at this price. A base-quantity
                        // residual still cancels both.
                        if stop.is_done() {
                            break;
                        }
                        if stop.is_dust_at(price, lot) {
                            continue;
                        }
                        // Cancel the maker on the held level for the same lockstep
                        // event + state + risk effects as CancelMaker (#95); level
                        // removal stays with the empty_price_levels drain below.
                        // #247: a failed maker cancel aborts the sweep
                        // (#240) with this level's pre-match in the prefix.
                        let maker_cancel = self.cancel_resting_maker_on_level(
                            price_level,
                            side.opposite(),
                            maker_order_id,
                            CancelReason::SelfTradePrevention,
                        );
                        if price_level.order_count() == 0 {
                            empty_price_levels.push(price);
                        }
                        if let Err(err) = maker_cancel {
                            sweep_error = Some(err);
                            break;
                        }
                        stp_taker_cancelled = true;
                        break;
                    }
                }
            }

            // --- Normal matching (no STP conflict or after CancelMaker cleanup) ---
            // The level's fold was reserved above, before the STP arms.
            if let Some(strandable) = strandable_makers.as_mut() {
                self.capture_strandable_makers(price_level, strandable);
            }
            // #230: park here in tests — after this level's capture and
            // before its match — so a competitor can be driven against both
            // windows: admitting a strandable maker into a level the sweep
            // has not reached, and cancelling one the sweep has already
            // captured (then reusing its id).
            #[cfg(test)]
            if let Some(hook) = self.level_interleave_hook.as_ref() {
                hook(price);
            }
            let mut price_level_match = price_level.match_order(
                qty_cap,
                order_id,
                TimeInForce::Gtc,
                taker_kind,
                taker_ts,
                &self.transaction_id_generator,
            );
            // #225: when STP ran for this level, the sweep may only fill
            // makers the verdict was taken on. `stp_orders` is only populated
            // while `stp_active`, hence the guard.
            if cfg!(debug_assertions)
                && stp_active
                && let Some(maker) = stp_snapshot_breach(&stp_orders, &price_level_match)
            {
                report_stp_snapshot_breach("sweep", price, maker);
            }
            let executed = level_executed(qty_cap, &price_level_match, price);

            if let Err(err) = self.process_level_match(
                &mut match_result,
                &mut price_level_match,
                qty_cap,
                &mut filled_orders,
                price,
                price_level,
                side,
                &mut empty_price_levels,
            ) {
                sweep_error = Some(err);
                break;
            }
            if let Err(err) = executed.and_then(|executed| stop.consume(executed, price)) {
                sweep_error = Some(err);
                break;
            }

            // Early exit if budget is exhausted
            if stop.is_done() {
                break;
            }
        }

        // Batch remove empty price levels
        let levels_removed = !empty_price_levels.is_empty();
        for price in &empty_price_levels {
            // #247: re-checked under the price's stripe, so a level a
            // concurrent submit refilled after the sweep emptied it stays.
            self.remove_level_if_empty(side.opposite(), *price);
        }
        if levels_removed {
            // Refresh the operational depth gauges now that levels may
            // have been removed. No-op when the `metrics` feature is
            // disabled.
            self.record_depth_metric();
        }

        // Batch remove filled orders from tracking and update state. Each entry
        // carries the maker's TRUE filled quantity (captured per-level in
        // `process_level_match`), so OrderStateTracker / lifecycle consumers and
        // any audit/risk reconciliation that sums filled quantity from terminal
        // events see the real executed amount instead of a `0` placeholder (#104).
        // `Filled { filled_quantity }` here is the executed quantity, which for
        // a removed non-auto-replenishing reserve maker is its visible tranche
        // only: `pricelevel` drops the hidden depth behind it rather than
        // refreshing (#230). Report that discard exactly as the aggressive
        // residual guard in `add_order_inner` does, so both sides of the trade
        // feed the same counter and the same `INFO` trace, distinguished by
        // `path`. `strandable_makers` is `None` on every sweep of a book that
        // never rested such a maker, so the lookup is skipped entirely there.
        // Sorted once so the per-maker lookup below is a binary search:
        // O((S + F) log S) for S captured and F filled, instead of the
        // O(S * F) linear scan. The report order is the `filled_orders`
        // order, which this does not touch.
        if let Some(strandable) = strandable_makers.as_mut() {
            strandable.sort_unstable_by_key(|(id, _)| id.as_bytes());
        }
        {
            // #294: per maker, the caller code (Clock, metrics, tracing)
            // runs first and the index release follows, as before; the
            // guard releases the rest if that caller code unwinds, so an
            // unwind leaves no ghost location. Event order is unchanged.
            let mut release = FilledMakerRelease {
                book: self,
                filled: &filled_orders,
                strandable: strandable_makers.as_deref(),
                next: 0,
            };
            for (filled_id, filled_quantity) in &filled_orders {
                self.track_state(
                    *filled_id,
                    OrderStatus::Filled {
                        filled_quantity: *filled_quantity,
                    },
                );
                let discarded = release
                    .strandable
                    .and_then(|strandable| find_strandable(strandable, *filled_id));
                if let Some(discarded_hidden) = discarded {
                    tracing::info!(
                        path = "maker",
                        order_id = %filled_id,
                        executed_quantity = *filled_quantity,
                        discarded_hidden_quantity = discarded_hidden,
                        "reserve maker removed: visible tranche exhausted without auto-replenishment"
                    );
                    crate::orderbook::metrics::record_reserve_hidden_discarded(discarded_hidden);
                }
                release.release_next(discarded.is_some());
            }
        }

        // Return vectors to pool for reuse. `stp_orders` only entered the pool
        // when STP was active; otherwise it is an empty, never-filled `Vec` that
        // is simply dropped.
        release_sweep_buffers(
            filled_orders,
            empty_price_levels,
            strandable_makers,
            stp_orders,
            stp_active,
        );

        // #240: a sweep stopped by a pricelevel failure. The bookkeeping above
        // already made the book's indices match the makers actually consumed;
        // `match_result` holds exactly the committed prefix. Hand it back as
        // an abort so the caller publishes it like a partial fill and never
        // rests the remainder.
        if let Some(source) = post_only_probe_error {
            return Err(self.reject_untouched(order_id, source));
        }
        if let Some(source) = sweep_error {
            return self.abort_sweep(order_id, &mode, match_result, source);
        }

        let no_fills = match_result.trades().as_vec().is_empty();

        // If STP cancelled the taker and no fills occurred at all, return STP error.
        // When partial fills happened, return Ok with the partial result so the
        // caller can see what was executed.
        if stp_taker_cancelled && no_fills {
            self.track_state(
                order_id,
                OrderStatus::Cancelled {
                    filled_quantity: 0,
                    reason: CancelReason::SelfTradePrevention,
                },
            );
            crate::orderbook::metrics::record_reject(
                crate::orderbook::reject_reason::RejectReason::SelfTradePrevention,
            );
            return Err(OrderBookError::SelfTradePrevented {
                mode: self.stp_mode,
                taker_order_id: order_id,
                user_id: taker_user_id,
            });
        }

        // Check for insufficient liquidity on market paths.
        if no_fills {
            match mode {
                MatchMode::BaseQty {
                    quantity,
                    limit_price: None,
                } => {
                    crate::orderbook::metrics::record_reject(
                        crate::orderbook::reject_reason::RejectReason::InsufficientLiquidity,
                    );
                    return Err(OrderBookError::InsufficientLiquidity {
                        side,
                        requested: quantity,
                        available: 0,
                    });
                }
                MatchMode::QuoteAmount { amount } => {
                    crate::orderbook::metrics::record_reject(
                        crate::orderbook::reject_reason::RejectReason::InsufficientLiquidity,
                    );
                    return Err(OrderBookError::InsufficientLiquidityNotional {
                        side,
                        requested: amount,
                        spent: 0,
                    });
                }
                MatchMode::BaseQty {
                    limit_price: Some(_),
                    ..
                } => {
                    // Limit orders that fail to match return Ok with an
                    // empty result — the unfilled portion becomes resting
                    // depth in the caller-driven flow.
                }
            }
        }

        // Normalize the quote-notional path so the public `MatchResult`
        // does not leak the `u64::MAX` working sentinel through
        // `remaining_quantity()`. The notional path measures progress in
        // quote currency, not base qty, so the natural meaning of
        // "remaining base qty" is zero — the residual the caller cares
        // about is `requested - executed_value`, available directly on
        // `MatchResult`. #246: a rebuild that fails (only an allocator
        // refusal can) no longer returns the unnormalized result as a
        // success: the committed trades come back as an abort.
        if matches!(mode, MatchMode::QuoteAmount { .. }) {
            match_result = match Self::normalize_notional_match_result(order_id, match_result) {
                Ok(normalized) => normalized,
                Err((unnormalized, source)) => {
                    Self::report_unnormalized_prefix(order_id, &source);
                    return self.abort_with_prefix(order_id, unnormalized, source);
                }
            };
        }

        Ok(MatchOutcome {
            result: match_result,
            taker_stp_cancelled: stp_taker_cancelled,
            taker_post_only_rejected: post_only_rejected,
            aborted: None,
        })
    }

    /// Rebuild a `MatchResult` produced by the quote-notional path so its
    /// internal `remaining_quantity` is `0` (rather than
    /// `u64::MAX - executed_qty`). Trade list, filled-order ids, and
    /// monotonic engine sequence stamping are preserved.
    ///
    /// The rebuilt result is sized up front (`try_with_capacity`), so the
    /// appends below never grow it. The trade quantities were each
    /// subtracted from the `u64::MAX` working budget, so their checked sum
    /// fits and every `add_trade` fits the rebuilt budget exactly.
    ///
    /// # Errors
    ///
    /// Returns the untouched `src` with the failure (#246) instead of
    /// silently handing `src` back as if it were normalized: an allocator
    /// refusal of the rebuilt buffers ([`PriceLevelError::CapacityExceeded`])
    /// or, as an invariant breach, an overflowing executed quantity or a
    /// refused append ([`PriceLevelError::InvalidOperation`]). The caller
    /// reports the committed trades as an abort.
    // Both variants carry a whole `MatchResult`; boxing the `Err` would add
    // an allocation to the failure path that exists for allocator refusals.
    #[allow(clippy::result_large_err)]
    fn normalize_notional_match_result(
        order_id: Id,
        src: MatchResult,
    ) -> Result<MatchResult, (MatchResult, PriceLevelError)> {
        let executed_qty = match src.executed_quantity() {
            Ok(quantity) => quantity,
            Err(err) => return Err((src, err)),
        };
        Self::rebuild_match_result(order_id, src, executed_qty)
    }

    /// Rebuild `src`'s trades and filled ids into a fresh result for an
    /// incoming quantity of `initial` (sized up front, so the appends never
    /// grow it). Shared by [`Self::normalize_notional_match_result`] and
    /// [`Self::strip_level_error`].
    ///
    /// # Errors
    ///
    /// The untouched `src` with the failure: an allocator refusal
    /// ([`PriceLevelError::CapacityExceeded`]) or a refused append
    /// ([`PriceLevelError::InvalidOperation`], an invariant breach).
    #[allow(clippy::result_large_err)]
    fn rebuild_match_result(
        order_id: Id,
        src: MatchResult,
        initial: Quantity,
    ) -> Result<MatchResult, (MatchResult, PriceLevelError)> {
        let capacity = src.trades().len().max(src.filled_order_ids().len());
        let mut rebuilt = match MatchResult::try_with_capacity(order_id, initial, capacity) {
            Ok(rebuilt) => rebuilt,
            Err(err) => return Err((src, err)),
        };
        let appended = src
            .trades()
            .as_vec()
            .iter()
            .try_for_each(|trade| rebuilt.add_trade(*trade))
            .and_then(|()| {
                src.filled_order_ids()
                    .iter()
                    .try_for_each(|filled_id| rebuilt.add_filled_order_id(*filled_id))
            });
        match appended {
            Ok(()) => Ok(rebuilt),
            Err(err) => Err((src, err)),
        }
    }

    /// Log a quote-notional result that could not be normalized (#246). The
    /// taker is reported as aborted; its `remaining_quantity()` still holds
    /// the working bound (`u64::MAX` minus the executed quantity), which the
    /// abort makes explicit instead of passing off as a normal result.
    #[cold]
    #[inline(never)]
    fn report_unnormalized_prefix(order_id: Id, err: &PriceLevelError) {
        tracing::error!(
            order_id = %order_id,
            error = %err,
            "quote-notional result could not be normalized; committed trades reported as an abort"
        );
    }

    /// Build the empty-book result. Market paths return a typed error;
    /// limit paths return `Ok` with a zero-trade `MatchResult` so the
    /// caller can rest the order.
    #[cold]
    fn empty_book_result(
        &self,
        side: Side,
        mode: &MatchMode,
        match_result: MatchResult,
    ) -> Result<MatchResult, OrderBookError> {
        match mode {
            MatchMode::BaseQty {
                quantity,
                limit_price: None,
            } => {
                crate::orderbook::metrics::record_reject(
                    crate::orderbook::reject_reason::RejectReason::InsufficientLiquidity,
                );
                Err(OrderBookError::InsufficientLiquidity {
                    side,
                    requested: *quantity,
                    available: 0,
                })
            }
            MatchMode::QuoteAmount { amount } => {
                crate::orderbook::metrics::record_reject(
                    crate::orderbook::reject_reason::RejectReason::InsufficientLiquidity,
                );
                Err(OrderBookError::InsufficientLiquidityNotional {
                    side,
                    requested: *amount,
                    spent: 0,
                })
            }
            MatchMode::BaseQty {
                limit_price: Some(_),
                ..
            } => Ok(match_result),
        }
    }

    /// Record every maker resting at `price_level` whose hidden depth this
    /// sweep would **strand** if it consumed the maker's visible tranche
    /// (#230): a [`OrderType::ReserveOrder`] with `auto_replenish == false`
    /// and hidden quantity behind it. `pricelevel` removes such a maker once
    /// its visible tranche is fully taken, dropping the hidden depth instead
    /// of refreshing from it, and the order body is gone by the time the
    /// match returns — so the amount has to be captured beforehand to be
    /// reportable at all.
    ///
    /// Appends to `out`; the caller drains it after the sweep, matching ids
    /// against the makers the level actually removed. Every level is matched
    /// at most once per sweep, so a maker is captured at most once.
    ///
    /// Cost:
    ///
    /// - On a book with no strandable maker resting — the overwhelmingly
    ///   common case, and the one `strandable_makers_resting` detects — this
    ///   is never called at all: `match_order_inner` reads the count once
    ///   per sweep and skips the buffer, the captures and the drain lookup
    ///   wholesale. While one does rest, a level with no hidden depth costs
    ///   one relaxed atomic load here and nothing else.
    /// - On a book that did rest one, every level holding hidden depth
    ///   (so any two-tranche kind, not just the strandable ones) pays a
    ///   full pass over the level's resting orders. `iter_orders` is
    ///   `DashMap::iter` upstream, which read-locks **every shard** of the
    ///   map regardless of how few orders rest at the level, so this pass
    ///   is **not** free and shows up in the tails. The flag exists to
    ///   confine it to the books where it can actually report something.
    ///   The `reserve_sweep_hdr` benchmark covers both arms; see `BENCH.md`
    ///   for the current figures.
    #[inline]
    fn capture_strandable_makers(
        &self,
        price_level: &std::sync::Arc<pricelevel::PriceLevel>,
        out: &mut Vec<(Id, u64)>,
    ) {
        if self.strandable_makers_resting.load(Ordering::Relaxed) == 0
            || price_level.hidden_quantity() == 0
        {
            return;
        }
        for order in price_level.iter_orders() {
            if let OrderType::ReserveOrder {
                hidden_quantity,
                auto_replenish: false,
                ..
            } = order.as_ref()
                && hidden_quantity.as_u64() > 0
            {
                out.push((order.id(), hidden_quantity.as_u64()));
            }
        }
    }

    /// Processes match results from a single price level, updating the
    /// aggregate match result and bookkeeping vectors.
    ///
    /// Extracted to avoid code duplication between the normal path and
    /// the STP safe-quantity pre-match path. Routes outbound `engine_seq`
    /// stamping through [`OrderBook::next_engine_seq`] so the minting
    /// contract has a single source of truth.
    ///
    /// The book's installed `risk_state` is consulted on every trade so
    /// the maker's per-account `resting_notional` (and `open_count` on
    /// full fill) is decremented. The hook is a no-op when no
    /// `RiskConfig` is installed, matching the rest of the risk plumbing.
    ///
    /// The makers the level consumed are always recorded in
    /// `filled_orders` and the risk / price-level listener hooks always
    /// run, because those mirror the level's real state whatever the
    /// aggregate can hold; they read `price_level_match` before it is
    /// folded. The level's committed trades are then folded into
    /// `match_result` **all or nothing** (#240) by [`Self::fold_level`]
    /// (`requested` is the quantity the level was asked for).
    ///
    /// # Errors
    ///
    /// Returns the level's own failure (`MatchResult::error()`, the root
    /// cause: pricelevel committed the prefix it reports and then stopped;
    /// PriceLevel#217 a poisoned level reports its refusal here too, with
    /// no trades) or, failing that, the refused fold. The caller stops the
    /// sweep on `Err`; every trade the level committed has been accounted
    /// for above.
    #[allow(clippy::too_many_arguments)]
    fn process_level_match(
        &self,
        match_result: &mut MatchResult,
        price_level_match: &mut MatchResult,
        requested: u64,
        filled_orders: &mut Vec<(Id, u64)>,
        price: u128,
        price_level: &std::sync::Arc<pricelevel::PriceLevel>,
        side: Side,
        empty_price_levels: &mut Vec<u128>,
    ) -> Result<(), PriceLevelError> {
        let mut first_error: Option<PriceLevelError> = None;
        let level_trade_count = price_level_match.trades().len();
        {
            let level_trades = price_level_match.trades().as_vec();

            // Process trades if any occurred
            if !level_trades.is_empty() {
                // #286: a pending trailing stop is evaluated against every
                // sweep's first and last print; record the first (one
                // relaxed load when no stop is pending).
                #[cfg(feature = "special_orders")]
                self.pending_stops
                    .record_print(self.last_trade_price(), price);
                // Update last trade price atomically
                self.last_trade_price.store(price);
                self.has_traded.store(true, Ordering::Relaxed);

                // Update per-account risk counters for the maker side of
                // every trade.
                for trade in level_trades {
                    self.risk_state.on_fill(
                        trade.maker_order_id(),
                        trade.quantity().as_u64(),
                        trade.price().as_u128(),
                    );
                }

                // Notify price level changes
                self.emit_level_changed(side.opposite(), price_level);
            }

            // Collect fully-consumed makers for batch removal, each with its
            // true filled quantity. Sum the maker's trades from THIS
            // per-level result, where `filled_order_ids()` and `trades()` are
            // kept consistent by pricelevel (an id is recorded only after its
            // trade is added) — so the recorded `Filled { filled_quantity }`
            // stays correct even if the aggregate fold is refused (#104).
            // Per-level trade counts are small; this is the cold path, not
            // the matching hot loop.
            for &filled_order_id in price_level_match.filled_order_ids() {
                // A maker's trades at one level sum to at most its own
                // quantity, so the checked sum cannot overflow; a `None` is an
                // invariant breach, reported rather than clamped.
                let filled_quantity = level_trades
                    .iter()
                    .filter(|trade| trade.maker_order_id() == filled_order_id)
                    .try_fold(0u64, |acc, trade| {
                        acc.checked_add(trade.quantity().as_u64())
                    });
                let filled_quantity = match filled_quantity {
                    Some(quantity) => quantity,
                    None => {
                        first_error.get_or_insert_with(|| PriceLevelError::InvalidOperation {
                            message: format!(
                                "filled quantity of maker {filled_order_id} overflows u64"
                            ),
                        });
                        0
                    }
                };
                // Reserved before the level was touched
                // (`reserve_level_fold`), so this push does not grow the
                // pooled buffer.
                filled_orders.push((filled_order_id, filled_quantity));
                // The maker left its level. `on_fill` above already released
                // a normally exhausted maker (no-op here); a
                // non-auto-replenishing reserve maker was removed with its
                // hidden tranche discarded (#230), and that remainder is
                // released now, in the same removal, instead of staying
                // booked forever (#243 review).
                self.risk_state.on_maker_removed(filled_order_id);
            }
        }

        // Check if price level is empty and mark for removal
        if price_level.order_count() == 0 {
            empty_price_levels.push(price);
        }

        // The level's own failure is the root cause: pricelevel committed
        // the prefix folded below and then stopped. Read it before the fold,
        // which drains the level result.
        let level_error = price_level_match.error().cloned();

        if let Err(err) = Self::fold_level(match_result, price_level_match, requested) {
            // The aggregate could not take this level's committed trades.
            // The level, the makers' risk and order state already reflect
            // them, the trade stream will not. This is the ONLY path on which
            // the streams can disagree (see `doc/panic-boundaries.md`): say
            // so loudly and count it so it is detectable in production.
            Self::bump_diagnostic_counter(&self.match_fold_failures, "match_fold_failures");
            crate::orderbook::metrics::record_match_fold_failure();
            tracing::error!(
                price,
                level_trade_count,
                error = %err,
                "committed trades of a price level could not be folded into the taker's result"
            );
            first_error.get_or_insert(err);
        }

        match level_error {
            Some(level_err) => Err(level_err),
            None => match first_error {
                Some(err) => Err(err),
                None => Ok(()),
            },
        }
    }

    /// Fold one level's result into the sweep's aggregate (#240,
    /// PriceLevel#219), all or nothing: on `Err` the aggregate is exactly
    /// as it was (a trade-by-trade append past its reserved room is the
    /// exception, and only an invariant breach can refuse one).
    ///
    /// - A level with no trades and no filled makers (nothing matchable, a
    ///   rejected self-match, a poisoned level) changes nothing: its
    ///   remaining quantity is what it was asked for.
    /// - A level asked for exactly the aggregate's remaining quantity is
    ///   absorbed with `MatchResult::try_absorb`: into an empty aggregate it
    ///   adopts the level's buffers (no allocation, cannot fail), otherwise
    ///   it appends into the room `reserve_level_fold` reserved. A level
    ///   that failed mid-match hands its error to the aggregate, which the
    ///   abort path strips again (`abort_sweep`).
    /// - Any other level (quote-notional, STP pre-match) is appended trade
    ///   by trade after reserving room for all of it.
    ///
    /// # Errors
    ///
    /// [`PriceLevelError::CapacityExceeded`] when the aggregate cannot grow
    /// (only possible past the reservation: replenishment trades beyond
    /// the per-level bound); [`PriceLevelError::InvalidOperation`] on an
    /// invariant breach (a trade the aggregate's remaining quantity cannot
    /// take).
    #[inline]
    fn fold_level(
        match_result: &mut MatchResult,
        level: &mut MatchResult,
        requested: u64,
    ) -> Result<(), PriceLevelError> {
        if level.trades().is_empty() && level.filled_order_ids().is_empty() {
            return Ok(());
        }
        if requested == match_result.remaining_quantity().as_u64() {
            return match_result.try_absorb(level);
        }
        let level_trades = level.trades().as_vec();
        let level_filled = level.filled_order_ids();
        match_result.try_reserve_trades(level_trades.len())?;
        match_result.try_reserve_filled_order_ids(level_filled.len())?;
        for trade in level_trades {
            match_result.add_trade(*trade)?;
        }
        for filled_order_id in level_filled {
            match_result.add_filled_order_id(*filled_order_id)?;
        }
        Ok(())
    }

    /// `true` when [`Self::fold_level`] will absorb a level asked for
    /// `match_qty` into `match_result` without needing any room: the
    /// aggregate holds nothing yet and the level gets exactly its remaining
    /// quantity, so `try_absorb` adopts the level's own buffers.
    #[inline]
    #[must_use]
    fn fold_adopts_level(match_result: &MatchResult, match_qty: u64) -> bool {
        match_result.trades().is_empty()
            && match_result.filled_order_ids().is_empty()
            && match_qty == match_result.remaining_quantity().as_u64()
    }

    /// Reserve what folding one level asked for `match_qty` can need
    /// (#240), before the level is touched: `min(resting makers,
    /// match_qty)` more slots in the pooled filled-maker buffer, and the
    /// same in the aggregate's trade and filled-id vectors unless the fold
    /// will adopt the level's own buffers ([`Self::fold_adopts_level`]:
    /// the first traded level of every base-quantity sweep, which is where
    /// PriceLevel#219 removed the extra allocation and the deep-level
    /// over-reservation). Each maker trades at most once unless a
    /// replenishing iceberg / reserve comes back with a fresh tranche, and
    /// every trade takes at least one unit.
    ///
    /// Residual (documented in `doc/panic-boundaries.md`): replenishment
    /// trades, and makers admitted to the level by a concurrent submit on
    /// the shared gate while it is being swept, can exceed this bound on a
    /// level after the first; the extra slots then grow during the fold,
    /// and only an allocator refusal at that point leaves the level's
    /// trades out of the aggregate. On deep levels past the first the bound
    /// still over-reserves (amortized, so capacity carries over to later
    /// levels).
    ///
    /// # Errors
    ///
    /// [`PriceLevelError::CapacityExceeded`] when a buffer cannot grow; the
    /// level has not been touched.
    #[inline]
    fn reserve_level_fold(
        match_result: &mut MatchResult,
        filled_orders: &mut Vec<(Id, u64)>,
        price_level: &pricelevel::PriceLevel,
        match_qty: u64,
    ) -> Result<(), PriceLevelError> {
        let makers = u64::try_from(price_level.order_count()).unwrap_or(u64::MAX);
        let steps = steps_to_usize(makers.min(match_qty))?;
        reserve_filled_makers(filled_orders, steps)?;
        if Self::fold_adopts_level(match_result, match_qty) {
            return Ok(());
        }
        match_result.try_reserve_trades(steps)?;
        match_result.try_reserve_filled_order_ids(steps)
    }

    /// Reserve the fill-or-kill preflight's `reservation` (#240) in the
    /// aggregate result and the pooled filled-maker buffer. Amortized
    /// growth: a no-op when the room is already there, and the pooled
    /// buffer is reused across sweeps.
    ///
    /// # Errors
    ///
    /// [`PriceLevelError::CapacityExceeded`] when a buffer cannot grow (or
    /// a count does not fit `usize`); nothing observable has changed.
    #[inline]
    fn reserve_sweep(
        match_result: &mut MatchResult,
        filled_orders: &mut Vec<(Id, u64)>,
        reservation: SweepReservation,
    ) -> Result<(), PriceLevelError> {
        let trades = steps_to_usize(reservation.trades)?;
        let filled_ids = steps_to_usize(reservation.filled_ids)?;
        match_result.try_reserve_trades(trades)?;
        match_result.try_reserve_filled_order_ids(filled_ids)?;
        reserve_filled_makers(filled_orders, filled_ids)
    }

    /// Record a taker rejected by a pricelevel failure before any mutation
    /// (#240) — the fill-or-kill preflight, a failed post-only probe, an
    /// exhausted trade-id generator — as a terminal `Rejected` state with
    /// the resource code (`CapacityExceeded` / `CounterExhausted`), the
    /// reject metric, and the typed
    /// [`OrderBookError::PriceLevelError`]. Latches trade-id exhaustion
    /// when that is the cause.
    #[cold]
    #[inline(never)]
    pub(crate) fn reject_untouched(&self, order_id: Id, source: PriceLevelError) -> OrderBookError {
        let err = OrderBookError::PriceLevelError(source);
        let reason = RejectReason::from(&err);
        tracing::warn!(
            order_id = %order_id,
            error = %err,
            "taker rejected by a pricelevel failure before any mutation; book untouched"
        );
        if self.transaction_id_generator.is_exhausted() {
            self.latch_trade_ids_exhausted();
        }
        self.track_state(order_id, OrderStatus::Rejected { reason });
        crate::orderbook::metrics::record_reject(reason);
        err
    }

    /// Turn a sweep stopped at a failed level into an aborted
    /// [`MatchOutcome`] (#240).
    ///
    /// `match_result` is the committed prefix. The taker is recorded as
    /// `Cancelled { filled_quantity: executed, reason: MatchAborted }` —
    /// the same shape as an IOC whose remainder is cancelled — so every
    /// submission API reports one lifecycle, and its remainder is never
    /// rested by any caller.
    ///
    /// # Errors
    ///
    /// Only when the prefix's executed quantity cannot be summed, which the
    /// `MatchResult` invariant (every fold is a checked subtraction from a
    /// `u64` budget) rules out; reported as
    /// [`OrderBookError::PriceLevelError`] rather than guessed.
    ///
    /// A quote-notional prefix is normalized first; if that fails the
    /// unnormalized prefix is reported (#246, see
    /// [`Self::report_unnormalized_prefix`]) and the original `source`
    /// stays the abort's cause.
    ///
    /// A base-quantity prefix whose failed level was absorbed with
    /// `MatchResult::try_absorb` (PriceLevel#219) carries that level's
    /// error in its error slot. The error is the abort's `source`; the
    /// published prefix is rebuilt without it, so the committed
    /// `TradeResult` has the same shape as before #219. If that rebuild is
    /// refused (only an allocator refusal can) the prefix is published with
    /// the error slot set rather than dropped, and logged.
    #[cold]
    #[inline(never)]
    fn abort_sweep(
        &self,
        order_id: Id,
        mode: &MatchMode,
        match_result: MatchResult,
        source: PriceLevelError,
    ) -> Result<MatchOutcome, OrderBookError> {
        let match_result = if matches!(mode, MatchMode::QuoteAmount { .. }) {
            match Self::normalize_notional_match_result(order_id, match_result) {
                Ok(normalized) => normalized,
                Err((unnormalized, err)) => {
                    Self::report_unnormalized_prefix(order_id, &err);
                    unnormalized
                }
            }
        } else if match_result.is_failed() {
            match Self::strip_level_error(order_id, match_result) {
                Ok(stripped) => stripped,
                Err((unstripped, err)) => {
                    tracing::error!(
                        order_id = %order_id,
                        error = %err,
                        "aborted prefix could not be rebuilt without the failed level's error; published with it"
                    );
                    unstripped
                }
            }
        } else {
            match_result
        };
        self.abort_with_prefix(order_id, match_result, source)
    }

    /// Rebuild `src` without its error slot: same trades, filled ids and
    /// remaining quantity (PriceLevel#219, see [`Self::abort_sweep`]).
    ///
    /// # Errors
    ///
    /// The untouched `src` with the failure, as for
    /// [`Self::normalize_notional_match_result`]; additionally
    /// [`PriceLevelError::InvalidOperation`] when executed plus remaining
    /// quantity overflows `u64` (an invariant breach).
    #[cold]
    #[inline(never)]
    #[allow(clippy::result_large_err)]
    fn strip_level_error(
        order_id: Id,
        src: MatchResult,
    ) -> Result<MatchResult, (MatchResult, PriceLevelError)> {
        let initial = match src.executed_quantity() {
            Ok(executed) => executed
                .as_u64()
                .checked_add(src.remaining_quantity().as_u64()),
            Err(err) => return Err((src, err)),
        };
        match initial {
            Some(initial) => Self::rebuild_match_result(order_id, src, Quantity::new(initial)),
            None => Err((
                src,
                PriceLevelError::InvalidOperation {
                    message: "aborted prefix quantity overflows u64".to_string(),
                },
            )),
        }
    }

    /// Second half of [`Self::abort_sweep`]: record and return the abort for
    /// a committed prefix that needs no (further) normalization.
    ///
    /// # Errors
    ///
    /// Same as [`Self::abort_sweep`].
    #[cold]
    #[inline(never)]
    fn abort_with_prefix(
        &self,
        order_id: Id,
        match_result: MatchResult,
        source: PriceLevelError,
    ) -> Result<MatchOutcome, OrderBookError> {
        let executed_quantity = match_result.executed_quantity()?.as_u64();
        let trade_count = match_result.trades().len();
        tracing::error!(
            order_id = %order_id,
            executed_quantity,
            trade_count,
            error = %source,
            "matching sweep aborted by a price level failure; committed prefix kept, remainder cancelled"
        );
        self.track_state(
            order_id,
            OrderStatus::Cancelled {
                filled_quantity: executed_quantity,
                reason: CancelReason::MatchAborted,
            },
        );
        crate::orderbook::metrics::record_reject(RejectReason::MatchAborted);
        crate::orderbook::metrics::record_match_abort();
        Self::bump_diagnostic_counter(&self.match_aborts, "match_aborts");
        if matches!(
            source,
            PriceLevelError::CapacityExceeded {
                resource: CapacityResource::IdSequence,
                ..
            }
        ) {
            self.latch_trade_ids_exhausted();
        }
        Ok(MatchOutcome {
            result: match_result,
            taker_stp_cancelled: false,
            taker_post_only_rejected: false,
            aborted: Some(OrderBookError::MatchAborted {
                order_id,
                executed_quantity,
                trade_count,
                source: Box::new(source),
            }),
        })
    }

    /// Optimized peek match without memory pooling or sorting: the quantity,
    /// in quantity units, of `quantity` that the raw resting depth (visible
    /// plus hidden) up to `price_limit` could absorb. It does not model
    /// self-trade prevention, lot rounding or undrawable reserve depth.
    ///
    /// # Performance Optimization
    /// Uses SkipMap's natural ordering to eliminate sorting overhead.
    /// Time complexity: O(M log N) where M = price levels inspected.
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::PriceLevelError`] when a level it reaches
    /// cannot report its depth because `visible + hidden` overflows `u64`
    /// (#246: such a level used to be read as empty).
    /// [`OrderBookError::InvalidOperation`] reports a breach of the
    /// `matched <= quantity` accounting invariant, which cannot happen.
    pub fn peek_match(
        &self,
        side: Side,
        quantity: u64,
        price_limit: Option<u128>,
    ) -> Result<u64, OrderBookError> {
        let price_levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        if price_levels.is_empty() {
            return Ok(0);
        }

        let mut matched_quantity = 0u64;

        // Iterate through prices in optimal order (already sorted by SkipMap)
        let price_iter = match side {
            Side::Buy => Either::Left(price_levels.iter()),
            Side::Sell => Either::Right(price_levels.iter().rev()),
        };

        // Process each price level
        for entry in price_iter {
            // Early termination when we have enough quantity
            if matched_quantity >= quantity {
                break;
            }

            let price = *entry.key();

            // Check price limit
            if let Some(limit) = price_limit {
                match side {
                    Side::Buy if price > limit => break,
                    Side::Sell if price < limit => break,
                    _ => {}
                }
            }

            // Get available quantity at this level. `matched_quantity <
            // quantity` here (loop head), and each level adds at most what
            // is still needed, so the checked steps below cannot fail.
            let price_level = entry.value();
            let available_quantity = price_level.total_quantity()?;
            let needed_quantity = quantity
                .checked_sub(matched_quantity)
                .ok_or_else(peek_accounting_breach)?;
            let quantity_to_match = needed_quantity.min(available_quantity);
            matched_quantity = matched_quantity
                .checked_add(quantity_to_match)
                .ok_or_else(peek_accounting_breach)?;
        }

        Ok(matched_quantity)
    }

    /// Faithful fill-or-kill feasibility: the quantity an immediate match of
    /// `quantity` would *actually* fill, mirroring the real walk in
    /// [`Self::match_order_inner`] — `lot_size` budget rounding, self-trade
    /// prevention, and per-order *drawable* depth (a non-auto-replenish reserve's
    /// hidden tranche is dropped unfilled by the sweep, so it is not counted).
    ///
    /// [`Self::peek_match`] only sums raw level depth, so it over-reports when STP
    /// would cancel makers / the taker, or when a non-replenish reserve's hidden is
    /// present. Routing FOK admission through this keeps fill-or-kill
    /// all-or-nothing: an order that cannot be fully filled is killed *before* any
    /// trade is emitted (#96).
    ///
    /// Alongside the fillable quantity the walk measures what the sweep will
    /// draw on (#240): the trade ids it takes and the result buffers it
    /// needs, so the fill-or-kill preflight can check the trade-id headroom
    /// and reserve the buffers before any mutation. See [`FokFeasibility`].
    ///
    /// Each level is dry-run with `PriceLevel::match_requirements` for the
    /// quantity the sweep will ask of it (PriceLevel#218), which reports the
    /// exact trades, trade ids and FIFO sequences the match takes. Under
    /// [`FeasibilityScope::Preflight`] every level the sweep reaches must
    /// also be matchable in full: not poisoned (PriceLevel#217), its
    /// counters with headroom (`MatchRequirements::check` against
    /// `PriceLevel::counter_headroom`), and no maker step the sweep would
    /// stop at (`MatchRequirements::stop_error`). The per-level answers are
    /// only valid while nothing else can mutate the levels until the sweep
    /// ends, which the exclusive submit gate a fill-or-kill submit holds
    /// guarantees: every `OrderBook` mutation path takes a side of that gate
    /// or `&mut self` (see [`Self::acquire_coherent_submit_gate`]).
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::PriceLevelError`] when a level's
    /// insertion-sequence view or its dry run fails (both fallible since
    /// pricelevel 0.10). The caller must treat that as "feasibility
    /// unknown" and refuse the order rather than guess: a failed dry run is
    /// a kill, never zero depth. Under [`FeasibilityScope::Preflight`] also
    /// returns the error of the first reached level that could not be
    /// matched in full: `InvalidOperation` for a poisoned level,
    /// `CounterExhausted` for a counter without headroom, or the maker-step
    /// error the sweep would stop at.
    pub(crate) fn fok_fillable_quantity(
        &self,
        side: Side,
        quantity: u64,
        price_limit: Option<u128>,
        taker_user_id: Hash32,
        taker_id: Id,
        scope: FeasibilityScope,
    ) -> Result<FokFeasibility, OrderBookError> {
        let price_levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };
        let mut feasibility = FokFeasibility {
            fillable: 0,
            trade_ids: 0,
            reservation: SweepReservation::NONE,
        };
        if quantity == 0 || price_levels.is_empty() {
            return Ok(feasibility);
        }

        let lot = self.lot_size.unwrap_or(1);
        let stp_active = self.stp_mode.is_enabled() && taker_user_id != Hash32::zero();
        let preflight = scope == FeasibilityScope::Preflight;

        let price_iter = match side {
            Side::Buy => Either::Left(price_levels.iter()),
            Side::Sell => Either::Right(price_levels.iter().rev()),
        };

        for entry in price_iter {
            if feasibility.fillable >= quantity {
                break;
            }

            let price = *entry.key();
            if let Some(limit) = price_limit {
                match side {
                    Side::Buy if price > limit => break,
                    Side::Sell if price < limit => break,
                    _ => {}
                }
            }

            // Lot-round the remaining budget exactly like `StopCondition::level_qty_cap`:
            // a budget below one full lot is dust and stops the walk.
            // `fillable < quantity` holds here (checked at the loop head).
            let Some(needed) = quantity.checked_sub(feasibility.fillable) else {
                break;
            };
            let cap = if lot <= 1 {
                needed
            } else {
                needed
                    .checked_rem(lot)
                    .and_then(|dust| needed.checked_sub(dust))
                    .unwrap_or(0)
            };
            if cap == 0 {
                break;
            }

            let price_level = entry.value();

            // PriceLevel#217: a poisoned level refuses every match. The
            // sweep would stop there, after the earlier levels committed.
            if preflight && price_level.is_poisoned() {
                return Err(OrderBookError::PriceLevelError(poisoned_level_error(price)));
            }
            #[cfg(test)]
            if preflight
                && let Some(hook) = self.fok_level_fault_hook.as_ref()
                && let Some(err) = hook(price)
            {
                return Err(OrderBookError::PriceLevelError(err));
            }

            // What the sweep will ask of this level, and what it takes.
            // `Some(match_qty)`: the level is matched once, for `match_qty`,
            // on the queue as it rests now, so pricelevel's dry run is exact
            // (non-STP, STP `NoConflict`, and the `CancelTaker` /
            // `CancelBoth` pre-match up to the non-self depth). `None`:
            // `CancelMaker` cancels same-user makers before the match, so
            // the dry run cannot see the queue the match will run on.
            //
            // The non-STP and STP-NoConflict depths delegate to pricelevel's
            // authoritative dry run (#136, follow-up to #96) instead of a
            // hand-rolled per-order estimate that could drift from
            // `match_against`.
            let (reachable, stop_after, exact_qty) = if stp_active {
                // Insertion-sequence order = the sweep's consumption order, so the
                // feasibility STP decision matches the real match even under
                // non-monotonic timestamps (#132).
                let orders = price_level.snapshot_by_insertion_seq()?;
                match check_stp_at_level(&orders, taker_user_id, self.stp_mode) {
                    // No self-trade: the whole level is reachable.
                    STPAction::NoConflict => (None, false, Some(cap)),
                    // Same-user makers are cancelled, not filled: only non-self
                    // resting depth is reachable; the walk continues. The upstream
                    // primitive cannot filter by user, so the non-self matchable
                    // depth is still summed per order here.
                    //
                    // #246: summed as `min(cap, Σ)` rather than `Σ`, since only
                    // `cap.min(reachable)` is used below. Depth summing past
                    // `u64::MAX` (legal: a visible and a hidden tranche near
                    // `u64::MAX` each) therefore reads as `cap` — exactly what the
                    // taker can take here — instead of panicking (debug) or
                    // wrapping into a false kill (release). Stops scanning once
                    // the cap is reached.
                    STPAction::CancelMaker => {
                        let mut non_self = 0u64;
                        for order in orders.iter().filter(|o| o.user_id() != taker_user_id) {
                            non_self = add_order_matchable_qty(non_self, order, cap);
                            if non_self == cap {
                                break;
                            }
                        }
                        (Some(non_self), false, None)
                    }
                    // The taker is cancelled at the first same-user order: the
                    // sweep pre-matches `min(cap, safe_quantity)` here, then
                    // stops. `safe_quantity` only sums the visible depth ahead
                    // of the same-user maker; what the pre-match takes is the
                    // dry run's fill for that request (a maker that makes no
                    // progress, or a replenish-overflow stop, delivers less
                    // than it shows), exactly as the modify precheck
                    // `check_modify_stp_self_cross` bounds it.
                    STPAction::CancelTaker { safe_quantity }
                    | STPAction::CancelBoth { safe_quantity, .. } => {
                        (None, true, Some(cap.min(safe_quantity)))
                    }
                }
            } else {
                (None, false, Some(cap))
            };

            let makers = u64::try_from(price_level.order_count()).unwrap_or(u64::MAX);
            // (taken, trade ids, trades to reserve, filled ids to reserve)
            let (taken, level_trade_ids, level_trades, level_filled) = match exact_qty {
                Some(match_qty) => {
                    let requirements = price_level.match_requirements(match_qty, taker_id)?;
                    if preflight && match_qty > 0 {
                        // PriceLevel#218: no per-level counter may refuse the
                        // match, and no maker step may stop it.
                        requirements
                            .check(&price_level.counter_headroom())
                            .map_err(OrderBookError::PriceLevelError)?;
                        if let Some(err) = requirements.stop_error() {
                            return Err(OrderBookError::PriceLevelError(err.clone()));
                        }
                    }
                    // The authoritative dry-run fill, never counted depth.
                    let taken = match_qty.min(requirements.fillable());
                    // The sweep reserves `min(makers, match_qty)` before it
                    // touches the level (`reserve_level_fold`); the fold then
                    // needs the exact trade count, which replenishments can
                    // push past that bound. Filled makers are at most the
                    // trades and the makers, so the level reservation covers
                    // them.
                    let level_reserve = makers.min(match_qty);
                    let trades = u64::try_from(requirements.trades()).unwrap_or(u64::MAX);
                    (
                        taken,
                        requirements.trade_ids_required()?,
                        trades.max(level_reserve),
                        level_reserve,
                    )
                }
                None => {
                    let taken = cap.min(reachable.unwrap_or(0));
                    if preflight
                        && let Some(counter) = price_level.counter_headroom().closed_epoch()
                    {
                        // Cancelling makers never reopens an epoch.
                        return Err(OrderBookError::PriceLevelError(
                            PriceLevelError::CounterExhausted { counter },
                        ));
                    }
                    let steps = makers.min(taken);
                    let ids_bound = if price_level.hidden_quantity() == 0 {
                        steps
                    } else {
                        taken
                    };
                    let level_reserve = makers.min(cap);
                    (taken, ids_bound, level_reserve, level_reserve)
                }
            };

            feasibility.fillable = feasibility
                .fillable
                .checked_add(taken)
                .ok_or_else(fok_counter_overflow)?;
            feasibility.trade_ids = feasibility
                .trade_ids
                .checked_add(level_trade_ids)
                .ok_or_else(fok_counter_overflow)?;
            feasibility.reservation.trades = feasibility
                .reservation
                .trades
                .checked_add(level_trades)
                .ok_or_else(fok_counter_overflow)?;
            feasibility.reservation.filled_ids = feasibility
                .reservation
                .filled_ids
                .checked_add(level_filled)
                .ok_or_else(fok_counter_overflow)?;
            if stop_after {
                break;
            }
        }

        Ok(feasibility)
    }

    /// Batch operation for multiple order matches (additional optimization)
    ///
    /// Each element follows [`Self::match_order`], including its #240
    /// asymmetry: an aborted partial fill is an `Err(MatchAborted)` with a
    /// summary only.
    pub fn match_orders_batch(
        &self,
        orders: &[(Id, Side, u64, Option<u128>)],
    ) -> Vec<Result<MatchResult, OrderBookError>> {
        let mut results = Vec::with_capacity(orders.len());

        for &(order_id, side, quantity, limit_price) in orders {
            let result = OrderBook::<T>::match_order(self, order_id, side, quantity, limit_price);
            results.push(result);
        }

        results
    }
}

#[cfg(test)]
mod stop_condition_tests {
    use super::*;

    #[test]
    fn test_base_qty_cap_no_lot_returns_remaining() {
        let stop = StopCondition::BaseQty { remaining: 1_000 };
        assert_eq!(stop.level_qty_cap(100, 1), 1_000);
    }

    #[test]
    fn test_base_qty_cap_rounds_down_to_lot() {
        let stop = StopCondition::BaseQty { remaining: 1_005 };
        // lot=100 ⇒ 1_005 - (1_005 % 100) = 1_005 - 5 = 1_000
        assert_eq!(stop.level_qty_cap(50, 100), 1_000);
    }

    #[test]
    fn test_base_qty_cap_zero_when_below_lot() {
        let stop = StopCondition::BaseQty { remaining: 5 };
        assert_eq!(stop.level_qty_cap(50, 100), 0);
    }

    #[test]
    fn test_quote_amount_cap_basic() {
        // 10_000 / 100 = 100 base
        let stop = StopCondition::QuoteAmount { remaining: 10_000 };
        assert_eq!(stop.level_qty_cap(100, 1), 100);
    }

    #[test]
    fn test_quote_amount_cap_dust_below_one_unit() {
        // remaining < level_price ⇒ 0
        let stop = StopCondition::QuoteAmount { remaining: 50 };
        assert_eq!(stop.level_qty_cap(100, 1), 0);
    }

    #[test]
    fn test_quote_amount_cap_lot_rounds_down() {
        // 1_400 / 100 = 14, lot=10 ⇒ 14 - (14 % 10) = 10
        let stop = StopCondition::QuoteAmount { remaining: 1_400 };
        assert_eq!(stop.level_qty_cap(100, 10), 10);
    }

    #[test]
    fn test_quote_amount_cap_zero_when_below_one_full_lot() {
        // 1_400 / 1_000 = 1, lot=10 ⇒ 1 - (1 % 10) = 0
        let stop = StopCondition::QuoteAmount { remaining: 1_400 };
        assert_eq!(stop.level_qty_cap(1_000, 10), 0);
    }

    #[test]
    fn test_quote_amount_cap_zero_price_is_zero_cap() {
        // Adversarial input: zero price should not divide-by-zero.
        let stop = StopCondition::QuoteAmount { remaining: 1_000 };
        assert_eq!(stop.level_qty_cap(0, 1), 0);
    }

    #[test]
    fn test_quote_amount_cap_bounded_at_u64_max() {
        // remaining = u128::MAX, level_price = 1 ⇒ derived qty would
        // exceed u64::MAX; the per-level request is bounded at u64::MAX,
        // the most a `u64` `MatchResult` can record (#246: `try_from`).
        let stop = StopCondition::QuoteAmount {
            remaining: u128::MAX,
        };
        assert_eq!(stop.level_qty_cap(1, 1), u64::MAX);
    }

    #[test]
    fn test_consume_base_qty_subtracts_executed() {
        let mut stop = StopCondition::BaseQty { remaining: 100 };
        assert!(stop.consume(30, 999).is_ok());
        assert!(matches!(stop, StopCondition::BaseQty { remaining: 70 }));
    }

    /// #246: an over-consumption used to saturate the budget at zero; it is
    /// now a typed invariant failure and the budget is left untouched.
    #[test]
    fn test_consume_base_qty_over_budget_is_an_error() {
        let mut stop = StopCondition::BaseQty { remaining: 5 };
        assert!(matches!(
            stop.consume(10, 999),
            Err(PriceLevelError::InvalidOperation { .. })
        ));
        assert!(matches!(stop, StopCondition::BaseQty { remaining: 5 }));
    }

    #[test]
    fn test_consume_quote_amount_deducts_price_times_qty() {
        let mut stop = StopCondition::QuoteAmount { remaining: 10_000 };
        assert!(stop.consume(30, 100).is_ok()); // spent = 100 * 30 = 3_000
        assert!(matches!(
            stop,
            StopCondition::QuoteAmount { remaining: 7_000 }
        ));
    }

    /// #246: spending more notional than remains, or a notional product
    /// that overflows `u128`, is a typed invariant failure; the budget is
    /// left untouched (it used to saturate at zero).
    #[test]
    fn test_consume_quote_amount_over_budget_is_an_error() {
        let mut stop = StopCondition::QuoteAmount { remaining: 100 };
        // spent = 10_000 > 100
        assert!(matches!(
            stop.consume(10, 1_000),
            Err(PriceLevelError::InvalidOperation { .. })
        ));
        assert!(matches!(
            stop,
            StopCondition::QuoteAmount { remaining: 100 }
        ));

        let mut stop = StopCondition::QuoteAmount {
            remaining: u128::MAX,
        };
        assert!(stop.consume(u64::MAX, u128::MAX).is_err());
        assert!(matches!(
            stop,
            StopCondition::QuoteAmount {
                remaining: u128::MAX
            }
        ));
    }

    #[test]
    fn test_is_done_base_qty() {
        assert!(StopCondition::BaseQty { remaining: 0 }.is_done());
        assert!(!StopCondition::BaseQty { remaining: 1 }.is_done());
    }

    #[test]
    fn test_is_done_quote_amount() {
        assert!(StopCondition::QuoteAmount { remaining: 0 }.is_done());
        assert!(!StopCondition::QuoteAmount { remaining: 1 }.is_done());
    }

    #[test]
    fn test_zero_cap_is_terminal_for_base_qty_on_both_sides() {
        // The base cap is the lot-rounded residual and ignores the level
        // price, so a zero cap stays zero whichever way the walk runs.
        let stop = StopCondition::BaseQty { remaining: 5 };
        assert_eq!(stop.level_qty_cap(50, 100), 0);
        assert_eq!(stop.level_qty_cap(1, 100), 0);
        assert!(stop.zero_cap_is_terminal(Side::Buy, 100));
        assert!(stop.zero_cap_is_terminal(Side::Sell, 100));
    }

    #[test]
    fn test_zero_cap_is_terminal_for_quote_amount_on_a_buy() {
        // Buy walks asks ascending: 50 cannot fund a unit at 100 and funds
        // even less at the dearer 200 the walk would visit next.
        let stop = StopCondition::QuoteAmount { remaining: 50 };
        assert_eq!(stop.level_qty_cap(100, 1), 0);
        assert_eq!(stop.level_qty_cap(200, 1), 0);
        assert!(stop.zero_cap_is_terminal(Side::Buy, 1));
    }

    #[test]
    fn test_zero_cap_is_not_terminal_for_quote_amount_on_a_sell() {
        // Sell walks bids descending: 50 cannot fund a unit at 75 but funds
        // exactly one at the cheaper 50 the walk would visit next, so the
        // unaffordable level must be skipped rather than end the walk.
        let stop = StopCondition::QuoteAmount { remaining: 50 };
        assert_eq!(stop.level_qty_cap(75, 1), 0);
        assert_eq!(stop.level_qty_cap(50, 1), 1);
        assert!(!stop.zero_cap_is_terminal(Side::Sell, 1));
    }

    #[test]
    fn test_zero_cap_is_not_terminal_for_a_lot_rounded_quote_sell() {
        // Same rule under lot rounding: 500 caps at 500/75 = 6, rounded
        // down to a lot of 5 that is still 5 — affordable. Push the price
        // to 200 and the cap is 2, which rounds to 0; at the cheaper 100
        // it is 5 again, so the walk must go on.
        let stop = StopCondition::QuoteAmount { remaining: 500 };
        assert_eq!(stop.level_qty_cap(200, 5), 0);
        assert_eq!(stop.level_qty_cap(100, 5), 5);
        assert!(!stop.zero_cap_is_terminal(Side::Sell, 5));
        assert!(stop.zero_cap_is_terminal(Side::Buy, 5));
    }

    #[test]
    fn test_quote_sell_terminal_boundary_is_remaining_below_one_lot() {
        // The exact bound. The cheapest a level can be is a price of 1,
        // where the cap is `remaining` itself rounded down to a lot, so a
        // sell walk is over precisely when `remaining < lot`.
        let lot = 5;
        let below = StopCondition::QuoteAmount { remaining: 4 };
        assert_eq!(below.level_qty_cap(1, lot), 0, "no price can fund a lot");
        assert!(below.zero_cap_is_terminal(Side::Sell, lot));

        let exactly_one_lot = StopCondition::QuoteAmount { remaining: 5 };
        assert_eq!(
            exactly_one_lot.level_qty_cap(1, lot),
            5,
            "a price of 1 funds exactly one lot"
        );
        assert!(!exactly_one_lot.zero_cap_is_terminal(Side::Sell, lot));
    }

    #[test]
    fn test_quote_sell_without_lot_size_is_terminal_only_on_a_spent_budget() {
        // `lot <= 1` collapses the bound to `remaining == 0`, which the
        // loop's own `is_done` check reaches first, so a notional sell then
        // stops only on an exhausted budget or an exhausted side.
        let spent = StopCondition::QuoteAmount { remaining: 0 };
        assert!(spent.zero_cap_is_terminal(Side::Sell, 1));
        assert!(spent.zero_cap_is_terminal(Side::Sell, 0));

        let one = StopCondition::QuoteAmount { remaining: 1 };
        assert!(!one.zero_cap_is_terminal(Side::Sell, 1));
        assert!(!one.zero_cap_is_terminal(Side::Sell, 0));
    }
}

#[cfg(test)]
// tests may panic: rules/global_rules.md § Testing
#[allow(clippy::arithmetic_side_effects)]
mod panic_forms_tests {
    //! #246: the panicking forms removed from matching, STP and the pool.
    use super::*;
    use crate::orderbook::stp::STPMode;
    use crate::orderbook::trade::TradeResult;
    use pricelevel::{Price, PriceLevel, TimestampMs, UuidGenerator};
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};

    fn user(byte: u8) -> Hash32 {
        Hash32::new([byte; 32])
    }

    fn standard(id: u64, quantity: u64, user_id: Hash32) -> OrderType<()> {
        OrderType::Standard {
            id: Id::from_u64(id),
            price: Price::new(100),
            quantity: Quantity::new(quantity),
            side: Side::Sell,
            user_id,
            timestamp: TimestampMs::new(0),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        }
    }

    /// Sum of every traded quantity reaching the trade listener, in `u128`
    /// so the test's own arithmetic cannot overflow.
    fn install_filled_sum(book: &mut OrderBook<()>) -> Arc<Mutex<u128>> {
        let sum = Arc::new(Mutex::new(0u128));
        let sink = Arc::clone(&sum);
        book.trade_listener = Some(Arc::new(move |result: &TradeResult| {
            let mut total = sink.lock().expect("sum mutex");
            for trade in result.match_result.trades().as_vec() {
                *total += u128::from(trade.quantity().as_u64());
            }
        }));
        sum
    }

    /// A level whose non-self matchable depth sums past `u64::MAX`: a
    /// visible tranche of 2^63 plus an iceberg of 2^62 visible / 2^63
    /// hidden (2^64 + 2^62 in total), behind which rests one unit of the
    /// taker's own. The `CancelMaker` fill-or-kill walk used to `.sum()`
    /// that depth: a panic in debug, a wrap to 2^62 (a false kill) in
    /// release. It now reads as the taker's cap, and the sweep fills it.
    #[test]
    fn test_fok_cancel_maker_depth_past_u64_max_is_fillable() {
        let taker_user = user(1);
        let other_user = user(2);
        let mut book: OrderBook<()> = OrderBook::new("FOK-EXTREME");
        book.set_stp_mode(STPMode::CancelMaker);
        let filled = install_filled_sum(&mut book);

        let big = 1u64 << 63;
        let quarter = 1u64 << 62;
        let limit = Id::from_u64(1);
        let iceberg = Id::from_u64(2);
        let own = Id::from_u64(3);
        let seeded = book.add_limit_order_with_user(
            limit,
            100,
            big,
            Side::Sell,
            TimeInForce::Gtc,
            other_user,
            None,
        );
        assert!(seeded.is_ok(), "seed limit: {seeded:?}");
        let seeded = book.add_iceberg_order_with_user(
            iceberg,
            100,
            quarter,
            big,
            Side::Sell,
            TimeInForce::Gtc,
            other_user,
            None,
        );
        assert!(seeded.is_ok(), "seed iceberg: {seeded:?}");
        let seeded = book.add_limit_order_with_user(
            own,
            100,
            1,
            Side::Sell,
            TimeInForce::Gtc,
            taker_user,
            None,
        );
        assert!(seeded.is_ok(), "seed own maker: {seeded:?}");

        let taker = Id::from_u64(10);
        let feasibility = book
            .fok_fillable_quantity(
                Side::Buy,
                u64::MAX,
                Some(100),
                taker_user,
                taker,
                FeasibilityScope::Preflight,
            )
            .expect("feasibility walk");
        assert_eq!(
            feasibility.fillable,
            u64::MAX,
            "depth past u64::MAX reads as the whole cap, not a wrapped remainder"
        );

        let submitted = book.add_limit_order_with_user(
            taker,
            100,
            u64::MAX,
            Side::Buy,
            TimeInForce::Fok,
            taker_user,
            None,
        );
        assert!(submitted.is_ok(), "the FOK is fillable: {submitted:?}");
        assert_eq!(
            *filled.lock().expect("sum mutex"),
            u128::from(u64::MAX),
            "the whole FOK quantity traded"
        );
        assert!(
            book.get_order(own).is_none(),
            "CancelMaker cancelled the taker's own maker"
        );
    }

    /// Resting depth whose `visible + hidden` overflows `u64` used to read
    /// as an empty level in `peek_match` (`total_quantity().unwrap_or(0)`);
    /// it is now a typed error.
    #[test]
    fn test_peek_match_overflowing_level_is_an_error() {
        let book: OrderBook<()> = OrderBook::new("PEEK-EXTREME");
        let seeded = book.add_limit_order(
            Id::from_u64(1),
            100,
            u64::MAX - 1,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
        assert!(seeded.is_ok(), "seed limit: {seeded:?}");
        let seeded = book.add_iceberg_order(
            Id::from_u64(2),
            100,
            1,
            5,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
        assert!(seeded.is_ok(), "seed iceberg: {seeded:?}");

        assert!(matches!(
            book.peek_match(Side::Buy, 10, None),
            Err(OrderBookError::PriceLevelError(_))
        ));
        // The other side is unaffected.
        assert_eq!(book.peek_match(Side::Sell, 10, None).ok(), Some(0));
    }

    /// The #225 invariant check that replaced the `debug_assert!`s: a maker
    /// outside the snapshot is found, a covered level is clean.
    #[test]
    fn test_stp_snapshot_breach_finds_a_maker_outside_the_snapshot() {
        let level = PriceLevel::new(100);
        let first = level.add_order(standard(1, 5, user(2))).expect("add first");
        let second = level
            .add_order(standard(2, 5, user(2)))
            .expect("add second");
        let generator = UuidGenerator::new(uuid::Uuid::nil());
        let level_match = level.match_order(
            10,
            Id::from_u64(9),
            TimeInForce::Gtc,
            TakerKind::Standard,
            TimestampMs::new(0),
            &generator,
        );
        assert_eq!(level_match.trades().len(), 2);

        assert_eq!(
            stp_snapshot_breach(&[Arc::clone(&first), Arc::clone(&second)], &level_match),
            None
        );
        assert_eq!(
            stp_snapshot_breach(&[first], &level_match),
            Some(second.id())
        );
    }

    /// Drives the breach end to end through the test seam: a maker slipped
    /// into the level behind the STP scan (bypassing the submit gate, which
    /// no production path can do) is filled by the `CancelMaker` sweep. The
    /// debug build used to panic on the `debug_assert!`; it now logs and
    /// the sweep completes with every committed trade.
    #[test]
    fn test_stp_snapshot_breach_is_reported_not_panicked() {
        let taker_user = user(1);
        let other_user = user(2);
        let mut book: OrderBook<()> = OrderBook::new("STP-BREACH");
        book.set_stp_mode(STPMode::CancelMaker);
        let filled = install_filled_sum(&mut book);
        let seeded = book.add_limit_order_with_user(
            Id::from_u64(1),
            100,
            5,
            Side::Sell,
            TimeInForce::Gtc,
            other_user,
            None,
        );
        assert!(seeded.is_ok(), "seed foreign: {seeded:?}");
        let own = Id::from_u64(2);
        let seeded = book.add_limit_order_with_user(
            own,
            100,
            5,
            Side::Sell,
            TimeInForce::Gtc,
            taker_user,
            None,
        );
        assert!(seeded.is_ok(), "seed own: {seeded:?}");

        let level = book
            .asks
            .get(&100)
            .map(|entry| Arc::clone(entry.value()))
            .expect("ask level");
        let injected = Id::from_u64(3);
        let armed = std::sync::atomic::AtomicBool::new(true);
        book.stp_interleave_hook = Some(Arc::new(move |_price: u128| {
            if armed.swap(false, Ordering::SeqCst) {
                let added = level.add_order(standard(3, 5, user(3)));
                assert!(added.is_ok(), "inject maker: {added:?}");
            }
        }));

        let result =
            book.submit_market_order_with_user(Id::from_u64(10), 10, Side::Buy, taker_user);
        let result = result.expect("the sweep completes");
        assert!(
            result
                .trades()
                .as_vec()
                .iter()
                .any(|trade| trade.maker_order_id() == injected),
            "the injected maker traded"
        );
        assert_eq!(*filled.lock().expect("sum mutex"), 10);
        assert!(book.get_order(own).is_none(), "own maker cancelled");
    }

    /// A failed notional normalization hands the source back with a typed
    /// error instead of silently returning it as if normalized.
    #[test]
    fn test_normalize_failure_returns_source_and_error() {
        let level = PriceLevel::new(100);
        level.add_order(standard(1, 5, user(2))).expect("add maker");
        let generator = UuidGenerator::new(uuid::Uuid::nil());
        let taker = Id::from_u64(9);
        let level_match = level.match_order(
            5,
            taker,
            TimeInForce::Gtc,
            TakerKind::Standard,
            TimestampMs::new(0),
            &generator,
        );
        let mut src = MatchResult::new(taker, Quantity::new(u64::MAX));
        for trade in level_match.trades().as_vec() {
            src.add_trade(*trade).expect("fold");
        }

        let normalized = OrderBook::<()>::normalize_notional_match_result(taker, src.clone())
            .expect("normalizes");
        assert_eq!(normalized.remaining_quantity().as_u64(), 0);
        assert_eq!(normalized.trades().len(), 1);

        // A trade that does not belong to the rebuilt result's taker is
        // refused by `add_trade`: the source comes back untouched.
        let Err((returned, err)) =
            OrderBook::<()>::normalize_notional_match_result(Id::from_u64(77), src)
        else {
            panic!("normalizing under a foreign taker id must fail");
        };
        assert!(matches!(err, PriceLevelError::InvalidOperation { .. }));
        assert_eq!(returned.remaining_quantity().as_u64(), u64::MAX - 5);
        assert_eq!(returned.trades().len(), 1);
    }

    /// Runs a crossing sweep from a thread-local destructor, after the
    /// thread's `MATCHING_POOL` was destroyed: `LocalKey::with` panicked
    /// there, `try_with` falls back to fresh buffers.
    #[test]
    fn test_sweep_from_tls_destructor_after_pool_teardown() {
        /// `(pool destroyed before the sweep, filled quantity)`.
        type Recorded = Option<(bool, Result<u64, OrderBookError>)>;
        struct SweepOnDrop {
            book: Arc<OrderBook<()>>,
            outcome: Arc<Mutex<Recorded>>,
        }
        impl Drop for SweepOnDrop {
            fn drop(&mut self) {
                let pool_destroyed = MATCHING_POOL.try_with(|_| ()).is_err();
                let result = self
                    .book
                    .submit_market_order(Id::from_u64(10), 5, Side::Buy)
                    .map(|result| {
                        result
                            .trades()
                            .as_vec()
                            .iter()
                            .map(|trade| trade.quantity().as_u64())
                            .sum::<u64>()
                    });
                *self.outcome.lock().expect("outcome mutex") = Some((pool_destroyed, result));
            }
        }
        thread_local! {
            static GUARD: RefCell<Option<SweepOnDrop>> = const { RefCell::new(None) };
        }

        let book: Arc<OrderBook<()>> = Arc::new(OrderBook::new("TLS-TEARDOWN"));
        let seeded =
            book.add_limit_order(Id::from_u64(1), 100, 20, Side::Sell, TimeInForce::Gtc, None);
        assert!(seeded.is_ok(), "seed: {seeded:?}");
        let outcome = Arc::new(Mutex::new(None));

        let thread_book = Arc::clone(&book);
        let thread_outcome = Arc::clone(&outcome);
        std::thread::spawn(move || {
            // Register the guard first and the pool second: thread-local
            // destructors run in reverse registration order on the
            // platforms this crate targets, so the pool is gone by the time
            // the guard sweeps.
            GUARD.with(|guard| {
                *guard.borrow_mut() = Some(SweepOnDrop {
                    book: thread_book,
                    outcome: thread_outcome,
                });
            });
            MATCHING_POOL.with(|_| ());
        })
        .join()
        .expect("teardown thread did not panic");

        let recorded = outcome.lock().expect("outcome mutex").take();
        let Some((pool_destroyed, result)) = recorded else {
            panic!("the guard's destructor did not run");
        };
        assert!(
            matches!(result, Ok(5)),
            "the sweep filled from fresh buffers: {result:?}"
        );
        // Documents the scenario actually exercised on this platform.
        assert!(pool_destroyed, "the pool was destroyed before the guard");
    }
}
