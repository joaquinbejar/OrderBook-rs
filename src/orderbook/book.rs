//! Core OrderBook implementation for managing price levels and orders

use super::cache::PriceLevelCache;
use super::clock::{Clock, MonotonicClock};
use super::error::OrderBookError;
use super::fees::FeeSchedule;
use super::iterators::{
    LevelInfo, LevelsInRange, LevelsUntilDepth, LevelsWithCumulativeDepth, analytics_overflow,
    checked_depth_add, checked_notional_add, level_total,
};
use super::market_impact::{MarketImpact, OrderSimulation};
use super::risk::{ReferencePriceSource, RiskConfig, RiskRebuild, RiskState};
use super::snapshot::{EnrichedSnapshot, MetricFlags, OrderBookSnapshot, OrderBookSnapshotPackage};
use super::statistics::{DepthStats, DistributionBin};
use super::user_orders::UserOrderIds;
use crate::orderbook::book_change_event::{PriceLevelChangedEvent, PriceLevelChangedListener};
use crate::orderbook::matching::{MatchOutcome, SweepReservation};
#[cfg(feature = "special_orders")]
use crate::orderbook::repricing::SpecialOrderTracker;
use crate::orderbook::stop_protection::StopProtection;
use crate::orderbook::stp::STPMode;
use crate::orderbook::trade::{SubmitFailure, TradeListener, TradeResult};
use crossbeam::atomic::AtomicCell;
use crossbeam_skiplist::SkipMap;
use dashmap::DashMap;
use either::Either;
use pricelevel::OrderUpdate;
use pricelevel::{
    Hash32, Id, MatchResult, OrderType, PriceLevel, PriceLevelError, PriceLevelSnapshot, Side,
    TakerKind, UuidGenerator,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use tracing::trace;
use uuid::Uuid;

/// Default basis points multiplier for spread calculations
/// One basis point = 0.01% = 0.0001
const DEFAULT_BASIS_POINTS_MULTIPLIER: f64 = 10_000.0;

/// Label hashed under [`Uuid::NAMESPACE_OID`] to obtain the root UUIDv5
/// namespace of every default trade-ID namespace (#265).
const DEFAULT_TRADE_ID_NAMESPACE_LABEL: &[u8] = b"orderbook-rs/default-trade-id-namespace";

/// Process-wide construction counter mixed into every default trade-ID
/// namespace (#265). Advanced with `checked_add`; see
/// [`default_trade_id_namespace`] for the exhaustion behaviour.
static DEFAULT_TRADE_ID_NAMESPACE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Takes the next value of a namespace construction counter: the value
/// before a `checked_add(1)`, or `u64::MAX` (logged at `WARN`, counter left
/// untouched) once the counter is exhausted. Never panics or wraps.
#[must_use]
pub(crate) fn next_namespace_seq(counter: &AtomicU64) -> u64 {
    match counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        current.checked_add(1)
    }) {
        Ok(previous) => previous,
        Err(exhausted) => {
            tracing::warn!(
                seq = exhausted,
                "default trade-id namespace counter exhausted; \
                 uniqueness now relies on the wall clock"
            );
            exhausted
        }
    }
}

/// Derives the default trade-ID namespace of a new book without reading OS
/// entropy (#265).
///
/// Every constructor that is not given a namespace used to call
/// `Uuid::new_v4()`, which reads OS entropy through `getrandom` and panics
/// when the RNG fails. The namespace does not need to be random or
/// cryptographic; it only needs to differ between book instances, in this
/// process and across restarts, so that two books never issue the same
/// trade ID. It is a UUIDv5 over non-panicking inputs:
///
/// 1. `symbol_ns = v5(root, symbol)`, where `root = v5(NAMESPACE_OID,
///    "orderbook-rs/default-trade-id-namespace")`;
/// 2. `namespace = v5(symbol_ns, pid ‖ wall_ns ‖ seq)`: the process id
///    ([`std::process::id`], 4 bytes LE), the wall clock in nanoseconds
///    since the UNIX epoch (16 bytes LE; `0` if the clock reads before the
///    epoch) and a process-wide construction counter (8 bytes LE).
///
/// Uniqueness argument:
///
/// - **Same process.** `seq` is taken with an atomic `try_update` +
///   `checked_add`, so every construction sees a distinct value, whatever
///   the thread, symbol or clock reading.
/// - **Concurrent processes.** Live processes have distinct pids.
/// - **Restarts.** A restarted process sees a later wall clock (and
///   usually a different pid), so `(pid, wall_ns, seq)` differs from every
///   tuple of the earlier run unless the clock was stepped back to the same
///   nanosecond while the OS handed out the same pid again. A clock that
///   reads before the epoch contributes `0`, leaving restart uniqueness to
///   the pid for as long as the clock stays broken.
/// - **Hash.** Distinct names map to distinct UUIDv5 values except for a
///   SHA-1 collision over 122 bits, negligible for non-adversarial input.
///
/// Counter exhaustion: after `u64::MAX` constructions in one process
/// (about 584 years at one book per nanosecond) `checked_add` refuses to
/// advance, `seq` stays at `u64::MAX`, a `WARN` is logged and uniqueness
/// then rests on the nanosecond wall clock alone. Nothing panics or wraps.
///
/// Replay determinism does not depend on this value: replay injects the
/// recorded namespace ([`OrderBook::set_trade_id_namespace`],
/// `ReplayBookConfig`), so the default only has to be unique.
#[must_use]
pub(crate) fn default_trade_id_namespace(symbol: &str) -> Uuid {
    let seq = next_namespace_seq(&DEFAULT_TRADE_ID_NAMESPACE_SEQ);
    let wall_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0u128, |elapsed| elapsed.as_nanos());
    let pid = std::process::id();

    let mut name = [0u8; 28];
    for (slot, byte) in name.iter_mut().zip(
        pid.to_le_bytes()
            .into_iter()
            .chain(wall_ns.to_le_bytes())
            .chain(seq.to_le_bytes()),
    ) {
        *slot = byte;
    }

    let root = Uuid::new_v5(&Uuid::NAMESPACE_OID, DEFAULT_TRADE_ID_NAMESPACE_LABEL);
    let symbol_ns = Uuid::new_v5(&root, symbol.as_bytes());
    Uuid::new_v5(&symbol_ns, &name)
}

/// Upper bound on the number of bins [`OrderBook::depth_distribution`]
/// builds (#245). A larger `bins` request is capped to this value, so the
/// histogram's allocation is bounded by a constant rather than by caller
/// input.
pub const MAX_DEPTH_DISTRIBUTION_BINS: usize = 4_096;

/// `engine_seq` carried by a caller-owned [`TradeResult`] whose trades
/// committed after the book's `engine_seq` was exhausted (#250).
///
/// [`OrderBook::next_engine_seq`] never mints `u64::MAX` (the last
/// mintable value is `u64::MAX - 1`), so this value is unambiguous: the
/// trades are real, the result was returned to an `add_order_with_result`
/// / `*_with_committed` caller, and no listener event was emitted for it.
pub const UNSTAMPED_ENGINE_SEQ: u64 = u64::MAX;

/// Where a resting order lives, plus its owner: the value of the book's
/// order-location index.
///
/// The owner is carried so the fill path can remove a filled maker from the
/// `user_orders` index with one keyed lookup: a filled maker has already left
/// its price level, so its body (and therefore its `user_id`) is no longer
/// reachable from the level (#259; before this the fill path scanned every
/// user's entry to find the id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OrderLocation {
    /// The price of the level the order rests on.
    pub(crate) price: u128,
    /// The side of the book the order rests on.
    pub(crate) side: Side,
    /// The owner the order is tracked under in `user_orders`.
    pub(crate) user_id: Hash32,
    /// The sequence the order is tracked under in its owner's
    /// `user_orders` entry, so removing it there is a binary search rather
    /// than a scan of every id the owner has resting.
    pub(crate) user_seq: u64,
}

impl OrderLocation {
    /// A location for an order resting at `price` on `side`, owned by
    /// `user_id` and tracked under `user_seq` in that owner's
    /// `user_orders` entry.
    #[must_use]
    #[inline]
    pub(crate) fn new(price: u128, side: Side, user_id: Hash32, user_seq: u64) -> Self {
        Self {
            price,
            side,
            user_id,
            user_seq,
        }
    }

    /// The `(price, side)` pair: the public shape of a location.
    #[must_use]
    #[inline]
    pub(crate) fn price_side(&self) -> (u128, Side) {
        (self.price, self.side)
    }
}

/// The OrderBook manages a collection of price levels for both bid and ask sides.
/// It supports adding, cancelling, and matching orders with lock-free operations where possible.
///
/// # Level statistics are advisory under concurrent takers (#241)
///
/// Every level snapshot the book hands out ([`create_snapshot`](Self::create_snapshot),
/// [`create_snapshot_package`](Self::create_snapshot_package),
/// [`snapshot_to_json`](Self::snapshot_to_json),
/// [`enriched_snapshot`](Self::enriched_snapshot),
/// [`enriched_snapshot_with_metrics`](Self::enriched_snapshot_with_metrics) and
/// `impl Serialize for OrderBook`) embeds pricelevel's per-level
/// `PriceLevelStatistics`, read through `PriceLevelSnapshot::statistics()`.
/// pricelevel 0.10 supports **exactly one concurrent writer** of a level's
/// execution aggregates (`orders_executed`, `quantity_executed`,
/// `value_executed`, `last_execution_time`, `sum_waiting_time`, and the
/// `stats_degraded` flag the recorder sets): its sequence guard protects
/// readers, it is not a writer lock.
///
/// The book does not serialize sweeps to provide that single writer. Two
/// sweeps that both hold the shared side of the submit gate may match at
/// the same price level at the same time, and each calls pricelevel's
/// `record_execution` for its own fills. That is the case for every
/// non-fill-or-kill taker and every matching-capable modify on an
/// [`STPMode::None`] book, and for anonymous (`Hash32::zero()`) match-only
/// sweeps such as [`match_order`](Self::match_order) under any mode, as long
/// as no strandable maker rests. Fill-or-kill takers, STP-relevant submits
/// and sweeps in a book holding a strandable maker run exclusively, so they
/// never overlap another recorder.
///
/// While two recorders overlap on one level, a snapshot of that level can
/// capture a **partial execution**: for example `orders_executed` already
/// counting a fill whose `quantity_executed` / `value_executed` has not
/// landed yet. The contract is therefore:
///
/// - **Exact regardless of concurrency:** trades, `MatchResult`,
///   `TradeResult` (fees included), every level's queue, quantities, order
///   count and order vector, and the order-admission / removal counters
///   (`orders_added`, `orders_removed`, plain atomic increments). None of them
///   is derived from the execution aggregates.
/// - **Exact once the overlapping sweeps return:** the execution aggregates
///   themselves. Every counter update is an atomic checked read-modify-write
///   and a rollback subtracts exactly what its own call added, so the next
///   snapshot taken with no sweep in flight on that level reads the true
///   totals. A snapshot read never hangs: the sequence is never left odd
///   once the writers stop.
/// - **Advisory while sweeps are in flight:** execution aggregates captured
///   concurrently with shared-gate takers may lag or be torn across fields.
///   Use them for monitoring, not for accounting or cross-field invariants
///   (for example `value_executed / quantity_executed` as an average price).
///   A snapshot package captured in that window checksums and, on
///   [`restore_from_snapshot_package`](Self::restore_from_snapshot_package),
///   installs those values verbatim; the checksum certifies integrity, not
///   coherence.
///
/// For exact statistics, capture snapshots while no sweep is in flight, or
/// drive the book from a single submitting thread (as a sequencer does).
/// Replay is single-threaded and always exact; see
/// `sequencer::snapshots_match` for why its statistics comparison is sound.
/// This is a documented trade-off (decision D6): serializing ordinary
/// sweeps would cost throughput on every book to fix a monitoring-only
/// field. The book-derived analytics ([`depth_statistics`](Self::depth_statistics),
/// the enriched snapshot metrics, market-impact simulation) read prices and
/// quantities only and are unaffected.
///
/// # Listener delivery (#249)
///
/// The trade, price-level and order-state listeners run **after** the
/// mutation that produced their events has committed and the submit gate
/// has been released, never mid-mutation or under a book lock. Per book,
/// deliveries form one total order consistent with commit order: the
/// `engine_seq` of trade and price-level events strictly increases across
/// the delivered stream, also with concurrent submitters, and a single
/// submitting thread sees exactly the order it saw before 0.14.0. Events
/// are delivered by whichever thread is dispatching: usually the submitter
/// before its call returns, but under concurrency possibly another thread,
/// after the submit returned. A listener may observe a book state newer
/// than its event, and may re-enter the book (its nested call's events are
/// delivered after the current batch). See [`TradeListener`] for the
/// details and [`Self::dropped_listener_events`] /
/// [`Self::listener_panics`] for the panicking-listener accounting.
///
/// # Caller-supplied code
///
/// `T` (`Clone`, `Default`, and whatever else the caller's type carries)
/// and every listener are caller code the crate cannot certify. They must
/// not panic. `T::default()` / `T::clone()` run at the book's boundary
/// (order conversion, snapshots), not inside pricelevel's matcher. A
/// `Clock`, the metrics recorder and the `tracing` subscriber also run
/// under the submit gate, mid-mutation. A panic in any of them unwinds out
/// of the calling entry point, and if that entry point held either side of
/// the submit gate the book engages its kill switch before releasing it
/// (see [`Self::submit_gate_poisoned`]). See `doc/panic-boundaries.md`.
pub struct OrderBook<T = ()> {
    /// The symbol or identifier for this order book
    pub(super) symbol: String,

    /// Bid side price levels (buy orders), stored in a concurrent ordered map (skip list)
    /// The map is keyed by price levels and stores Arc references to PriceLevel instances
    /// Using SkipMap provides O(log N) operations with automatic ordering, eliminating
    /// the need to sort prices during matching (optimization from O(N log N) to O(M log N))
    pub(super) bids: SkipMap<u128, Arc<PriceLevel>>,

    /// Ask side price levels (sell orders), stored in a concurrent ordered map (skip list)
    /// The map is keyed by price levels and stores Arc references to PriceLevel instances
    /// Using SkipMap provides O(log N) operations with automatic ordering, eliminating
    /// the need to sort prices during matching (optimization from O(N log N) to O(M log N))
    pub(super) asks: SkipMap<u128, Arc<PriceLevel>>,

    /// A concurrent map from order ID to its [`OrderLocation`] (price, side
    /// and owner) for fast lookups. This avoids having to search through all
    /// price levels to find an order, and lets a fill untrack the maker from
    /// `user_orders` by key instead of scanning every user (#259).
    pub(super) order_locations: DashMap<Id, OrderLocation>,

    /// A concurrent map from user ID to their order IDs for fast lookup.
    /// Maintained by `add_order`, `cancel_order`, and the matching engine
    /// to enable O(1) user-based mass cancellation.
    pub(super) user_orders: DashMap<Hash32, UserOrderIds>,

    /// Generator for unique transaction IDs
    pub(super) transaction_id_generator: UuidGenerator,

    /// Strictly monotonic sequence counter minted by [`Self::next_engine_seq`]
    /// and stamped on every outbound event (`TradeResult`,
    /// `PriceLevelChangedEvent`) so consumers can perform cross-stream gap
    /// detection and temporal ordering. Per `OrderBook<T>` instance — replay
    /// into a fresh book yields fresh seqs, not the originals.
    pub(super) engine_seq: AtomicU64,

    /// Operational kill switch. When `true`, every public `submit_*`,
    /// `add_order`, and non-cancel `update_order` call short-circuits with
    /// [`OrderBookError::KillSwitchActive`] before any matching, fee, STP,
    /// or allocation work happens. Cancel and mass-cancel paths are
    /// explicitly **not** gated so operators can drain the resting book.
    /// Persisted across snapshot/restore via
    /// [`OrderBookSnapshotPackage::kill_switch_engaged`](super::snapshot::OrderBookSnapshotPackage::kill_switch_engaged).
    pub(super) kill_switch: AtomicBool,

    /// Pre-trade risk state: optional [`RiskConfig`] plus per-account
    /// counters and per-order entries. When the embedded config is
    /// `None` (default), every check is a passthrough and every hook
    /// is a no-op. Always present so that [`Self::set_risk_config`]
    /// can engage the gates without constructor changes. The config
    /// is persisted across snapshot/restore; counters are rebuilt
    /// post-restore by walking the snapshot's resting orders.
    pub(super) risk_state: RiskState,

    /// The last price at which a trade occurred
    pub(super) last_trade_price: AtomicCell<u128>,

    /// Flag indicating if there was a trade
    pub(super) has_traded: AtomicBool,

    /// How many `ReserveOrder { auto_replenish: false, .. }` makers carrying
    /// hidden quantity are resting on this book (#230).
    ///
    /// Gates the pre-match scan that captures makers whose hidden depth a
    /// sweep would strand: at zero the scan can never report anything, and
    /// skipping it keeps the sweep off `PriceLevel::iter_orders`, whose
    /// `DashMap` iterator read-locks every shard per level match.
    /// `match_order_inner` reads this once per sweep and, when it is zero,
    /// allocates no capture buffer and runs no per-level capture at all.
    ///
    /// # Why the count is exact
    ///
    /// Not "an error would be harmless": the count is exact, and each side
    /// of it has a reason.
    ///
    /// **Increments** sit at the only two places an order is ever rested:
    /// the level insertion in `add_order_inner` and the snapshot-restore
    /// commit. A strandable maker cannot reach a level without passing one
    /// of them, so the count can never under-count.
    ///
    /// **Decrements** sit at the only three places such a maker leaves a
    /// level, and each decides from the removed order's **own body**, never
    /// from a cached id: `cancel_order_with_reason` (the funnel for user
    /// cancels, the three cancel-then-add modifies, the scoped mass cancels
    /// and expiry eviction) and `cancel_resting_maker_on_level` (the
    /// self-trade-prevention maker cancel) both hold the cancelled
    /// `OrderType`; the fill drain in `match_order_inner` decides from that
    /// sweep's own capture list. `cancel_all_orders` empties the book in
    /// bulk without that funnel and resets the count to zero instead.
    ///
    /// The fill drain is exact because of the gate rule in
    /// [`acquire_coherent_submit_gate`](Self::acquire_coherent_submit_gate):
    /// a sweep in a book holding strandable makers runs **exclusively**, so
    /// no cancel, admission or id reuse can interleave between a level's
    /// capture and its match. The capture therefore still describes the
    /// orders the match consumes, and an id in both the capture list and
    /// `filled_orders` is the same order in both — not a `Standard` order
    /// that reused a cancelled reserve's id.
    ///
    /// The checked decrement (a refused underflow, logged at `WARN`, #250)
    /// is defence in depth against a future path that removes a maker
    /// without passing one of the three, not a licence to be approximate.
    ///
    /// Not part of the snapshot format: the restore commit resets it to
    /// zero with the rest of the book state and recounts from the orders it
    /// installs.
    ///
    /// Coherence with the sweep (#225 / #230): admitting a strandable maker
    /// takes the **exclusive** side of the submit gate, so no such maker can
    /// be admitted while a sweep holds the shared side. The once-per-sweep
    /// read and the per-level captures therefore observe the same set.
    pub(super) strandable_makers_resting: AtomicUsize,

    /// Matching sweeps aborted by a failed price level since the book was
    /// built (#240). Diagnostic only: not part of the snapshot format.
    pub(super) match_aborts: AtomicU64,

    /// Price levels whose committed trades could not be folded into the
    /// taker's result (#240): the only path where the trade stream can
    /// disagree with the book, risk and order-state streams. Diagnostic
    /// only: not part of the snapshot format.
    pub(super) match_fold_failures: AtomicU64,

    /// Latched the first time the book finds its trade-id generator
    /// exhausted (#240). A latched book cannot trade again until its
    /// generator is replaced. Not part of the snapshot format.
    pub(super) trade_ids_exhausted: AtomicBool,

    /// Latched the first time an emission path finds `engine_seq`
    /// exhausted (#250); from then on outbound events are suppressed
    /// rather than stamped with a wrapped sequence. Cleared by a
    /// snapshot-package restore, which installs a fresh (validated)
    /// counter. Not part of the snapshot format.
    pub(super) engine_seq_exhausted: AtomicBool,

    /// The timestamp of market close, if applicable (for DAY orders)
    pub(super) market_close_timestamp: AtomicU64,

    /// Flag indicating if market close is set
    pub(super) has_market_close: AtomicBool,

    /// A cache for storing best bid/ask prices to avoid recalculation
    pub(super) cache: PriceLevelCache,

    /// Book-level linearization gate for decisions that span two
    /// operations: multi-level fill-or-kill (#209) and self-trade
    /// prevention (#225).
    ///
    /// Every mutating entry point takes a side of this gate. A fill-or-kill
    /// submit takes the **write** side across its feasibility check and
    /// sweep, and an STP-relevant submit or matching-capable modify takes it
    /// across its per-level scan and the fill that scan authorises, so no
    /// concurrent add / cancel / update can invalidate either decision
    /// between the two steps. Every mass cancel and expiry eviction takes
    /// the **write** side too (#248): each collects its scope before it
    /// removes it, and `cancel_all_orders` clears the tracking maps
    /// wholesale. While a trailing stop is pending (#286) every call that
    /// can trade, and every call that targets a pending stop, takes the
    /// **write** side, so the stops are only ever touched (and evaluated
    /// after a trade) by one call at a time; post-only adds and the
    /// quantity updates and cancels of other orders keep the read side.
    /// Everything else takes the **read** side and stays fully concurrent. [`OrderBook::acquire_coherent_submit_gate`] is the
    /// single place that picks the mode, and it documents the scope
    /// limitation.
    ///
    /// # Cost of enabling STP
    ///
    /// On an `STPMode::None` book the exclusive side is taken only by
    /// fill-or-kill submits, mass cancels and expiry eviction, which are
    /// rare. **Enabling any other
    /// [`STPMode`] serializes the book**: `validate_order_shape` rejects a
    /// zero `user_id` with [`OrderBookError::MissingUserId`], so every
    /// admissible `add_order` carries an identity, and every one of them
    /// that can take liquidity — plus every `UpdatePrice` /
    /// `UpdatePriceAndQuantity` / `Replace` and every market sweep that
    /// names a user — runs one at a time on that book. Only post-only
    /// submits (which never reach the STP scan), `UpdateQuantity`, cancels
    /// and anonymous match-only sweeps keep the shared side.
    /// This is the price of the #225 guarantee and it should be weighed
    /// before turning STP on for a hot symbol.
    ///
    /// This is deliberately `std::sync::RwLock` — a skiplist / dashmap is
    /// the wrong shape for an exclusion window (see CLAUDE.md). The
    /// matching hot path itself stays lock-free; the gate wraps entry
    /// points only and is never held across `.await` (the core is
    /// synchronous).
    pub(super) submit_gate: std::sync::RwLock<()>,

    /// Latched the first time code panicked while holding either side of
    /// the submit gate (#249, #294): detected by the guard's drop while
    /// the thread unwinds, and by an acquisition that finds the exclusive
    /// side poisoned. Since listeners run after the gate is released, that
    /// means engine code or caller code running mid-mutation (`Clock`,
    /// metrics recorder, `tracing` subscriber, `T::default()` /
    /// `T::clone()`) panicked; the book then engages the kill switch (see
    /// [`Self::submit_gate_poisoned`]). Not part of the snapshot format
    /// (the kill switch it engages is).
    pub(super) submit_gate_poisoned: AtomicBool,

    /// Sequenced outbox and dispatcher state for the trade, price-level and
    /// order-state listeners (#249): events produced under the submit gate
    /// are stamped into it at commit and delivered after the gate is
    /// released, in commit order. See `emission.rs`. Runtime-only.
    pub(super) outbox: super::emission::EventOutbox,

    /// Striped per-price reader-writer lock ordering the two operations
    /// that can race on a price level's **existence** under the shared
    /// submit gate (#247, Copilot on #285): admitting an order into a level
    /// (`get_or_insert` + `PriceLevel::add_order`) and removing a level that
    /// became empty from the bid / ask map.
    ///
    /// Without it, a remover could read `order_count() == 0`, a concurrent
    /// submit could then admit into the same `Arc<PriceLevel>` it had just
    /// looked up, and the remover's `SkipMap::remove` would unlink the level
    /// with that live order inside: indexed in `order_locations` but
    /// unreachable through `bids` / `asks`.
    ///
    /// Admissions take the **shared** side, so any number of them proceed
    /// in parallel at the same price, exactly as pricelevel's sharded level
    /// allows (a mutex here serialised a hot touch price: 1.7x to 4.5x
    /// slower concurrent adds at 2 to 16 threads). Removing an emptied level
    /// takes the **exclusive** side, which waits out in-flight admissions at
    /// that stripe and excludes new ones while it re-checks emptiness and
    /// unlinks; see [`Self::remove_level_if_empty`]. Removals are rare
    /// (only a level that just became empty) and short.
    ///
    /// Matching and in-place updates never add orders and take no stripe.
    /// A stripe is held for one level operation only, never across a sweep,
    /// a listener call or another lock, and never upgraded (an admission
    /// that must clean up drops its shared guard before taking the
    /// exclusive one), so it cannot deadlock. `std::sync::RwLock<()>`
    /// because no lock-free structure expresses "exclude admissions while
    /// unlinking"; poisoning is logged and recovered (see
    /// [`Self::lock_level`]).
    pub(super) level_locks: [std::sync::RwLock<()>; LEVEL_LOCK_STRIPES],

    /// Test-only interleaving point for the per-level self-trade-prevention
    /// scan (#225).
    ///
    /// Fires inside `match_order_with_user_outcome` right after
    /// `check_stp_at_level` has produced its verdict for a price level and
    /// before that verdict is acted on, receiving the level's price. It lets
    /// a unit test park the taker exactly inside the former check-then-act
    /// window and drive a competing thread against it deterministically,
    /// with no sleeps. The field exists only in `cfg(test)` builds, so
    /// neither the hook nor its `Option` check reaches a release binary.
    #[cfg(test)]
    pub(super) stp_interleave_hook: Option<std::sync::Arc<dyn Fn(u128) + Send + Sync>>,

    /// Test-only interleaving hook for the **normal** matching path (#230).
    ///
    /// [`Self::stp_interleave_hook`] fires inside the self-trade-prevention
    /// block, so it never runs on an [`STPMode::None`](super::stp::STPMode)
    /// book. This one fires just before each crossing level is matched,
    /// receiving that level's price, which lets a test park a plain sweep
    /// between two levels and drive a competing admission against it. Like
    /// its sibling it exists only in `cfg(test)` builds, so neither the hook
    /// nor its `Option` check reaches a release binary.
    #[cfg(test)]
    pub(super) level_interleave_hook: Option<std::sync::Arc<dyn Fn(u128) + Send + Sync>>,

    /// Test-only fault injection for the fill-or-kill preflight (#293).
    ///
    /// Consulted by `fok_fillable_quantity` under
    /// `FeasibilityScope::Preflight` for every level the walk reaches,
    /// after the real poisoned-level check: returning `Some(err)` makes the
    /// preflight treat that level as unmatchable with `err`. It stands in
    /// for states pricelevel does not let this crate construct: a poisoned
    /// level (PriceLevel#217 needs a panic inside the level) and a per-level
    /// counter without headroom (PriceLevel#218 needs about `2^64`
    /// operations on one level). Exists only in `cfg(test)` builds.
    #[cfg(test)]
    pub(super) fok_level_fault_hook:
        Option<std::sync::Arc<dyn Fn(u128) -> Option<pricelevel::PriceLevelError> + Send + Sync>>,

    /// Test-only fault injection for the single-order cancel path (#248).
    ///
    /// Consulted by `cancel_order_with_reason` right before it asks the
    /// order's price level to remove the order; returning a [`CancelFault`]
    /// makes that removal fail either with nothing mutated (a refusing
    /// level) or after the level committed it (a level that removes the
    /// order and then reports a broken invariant). pricelevel 0.10 has no
    /// public way to trigger either on demand, so this is what lets the
    /// mass-cancel failure paths be tested. Like its siblings it exists only
    /// in `cfg(test)` builds.
    #[cfg(test)]
    pub(super) cancel_fault_hook:
        Option<std::sync::Arc<dyn Fn(Id) -> Option<CancelFault> + Send + Sync>>,

    /// Test-only fault injection for resting an order on its price level
    /// (#247).
    ///
    /// Consulted right before the book asks a level to admit an order that
    /// is about to rest (a submit's remainder, a modify's re-add, or the
    /// restore of a modify's original); returning an error makes that
    /// admission fail with nothing mutated. pricelevel 0.10 has no public
    /// way to make a level refuse an admission on demand. Like its siblings
    /// it exists only in `cfg(test)` builds.
    #[cfg(test)]
    pub(super) modify_interleave_hook: Option<ModifyInterleaveHook<T>>,

    /// Test-only fault injection for resting an order on its price level
    /// (#247); see `rest_fault_hook`. The sibling `modify_interleave_hook`
    /// fires inside a cancel-then-add modify just before and just after the
    /// cancel, with the book, so a test can land a concurrent mutation in
    /// either window.
    #[cfg(test)]
    pub(super) rest_fault_hook:
        Option<std::sync::Arc<dyn Fn(Id) -> Option<pricelevel::PriceLevelError> + Send + Sync>>,

    /// Test-only interleaving point right after a level admitted an order
    /// that is resting (#288), receiving its id. From that point the order
    /// is matchable by a concurrent sweep, so a test can park the resting
    /// thread there and consume the order from another thread. Like its
    /// siblings it exists only in `cfg(test)` builds.
    #[cfg(test)]
    pub(super) rest_interleave_hook: Option<std::sync::Arc<dyn Fn(Id) + Send + Sync>>,

    /// Test-only fault injection for the risk reservation of an order about
    /// to rest (#291), receiving its id. Returning an error makes the
    /// reservation fail with nothing reserved, as a concurrent admission on
    /// the same account can make it fail after the pre-trade check passed.
    /// Like its siblings it exists only in `cfg(test)` builds.
    #[cfg(test)]
    pub(super) rest_risk_fault_hook:
        Option<std::sync::Arc<dyn Fn(Id) -> Option<OrderBookError> + Send + Sync>>,

    /// Test-only interleaving point in `UpdateQuantity` (#311), fired with
    /// the order's id after the update was validated against the resting
    /// order and right before the level applies it. A test can park the
    /// update there and consume the order from another thread, so the level
    /// reports it absent. Like its siblings it exists only in `cfg(test)`
    /// builds.
    #[cfg(test)]
    pub(super) update_interleave_hook: Option<std::sync::Arc<dyn Fn(Id) + Send + Sync>>,

    /// Test-only interleaving point in the repricers (#291), fired with the
    /// book and a tracked id right after `get_order` found no order for it
    /// and before the tracker entry is released, so a test can rest a
    /// same-id order in that window. Like its siblings it exists only in
    /// `cfg(test)` builds.
    #[cfg(all(test, feature = "special_orders"))]
    pub(super) reprice_interleave_hook: Option<RepriceInterleaveHook<T>>,

    /// listens to possible trades when an order is added
    pub trade_listener: Option<TradeListener>,

    /// Phantom data to maintain generic type parameter
    _phantom: PhantomData<T>,

    /// listens to order book changes. This provides a point to update a corresponding external order book e.g. in the UI
    pub price_level_changed_listener: Option<PriceLevelChangedListener>,

    /// Tracker for pegged orders, which re-price against the book.
    #[cfg(feature = "special_orders")]
    pub(super) special_order_tracker: SpecialOrderTracker,

    /// Pending trailing stops, held off book and triggered by the last
    /// trade price (#286); see `stop_orders.rs`. Part of the snapshot
    /// format (version 5).
    #[cfg(feature = "special_orders")]
    pub(super) pending_stops: super::stop_orders::PendingStops,

    /// Minimum price increment for orders. When set, order prices must be
    /// exact multiples of this value. `None` disables validation (default).
    pub(super) tick_size: Option<u128>,

    /// Minimum quantity increment for orders. When set, order quantities must be
    /// exact multiples of this value. `None` disables validation (default).
    pub(super) lot_size: Option<u64>,

    /// Minimum order size. When set, orders with `total_quantity() < min` are
    /// rejected. `None` disables validation (default).
    pub(super) min_order_size: Option<u64>,

    /// Maximum order size. When set, orders with `total_quantity() > max` are
    /// rejected. `None` disables validation (default).
    pub(super) max_order_size: Option<u64>,

    /// Self-Trade Prevention mode. When set to a mode other than `None`,
    /// the matching engine checks `user_id` on incoming and resting orders
    /// to prevent self-trades. Default is `STPMode::None` (disabled).
    pub(super) stp_mode: STPMode,

    /// Fee schedule for calculating trading fees. When None, no fees are applied.
    /// Fees are calculated during trade execution and can be configured per orderbook.
    pub(super) fee_schedule: Option<FeeSchedule>,

    /// Protection collar for elected stop orders (#302). `None` (the
    /// default) keeps the unpriced IOC market child; `Some` turns it into
    /// an IOC limit at the stop price moved by the collar. Read only when a
    /// stop is elected; part of the snapshot package (format version 6).
    pub(super) stop_protection: Option<StopProtection>,

    /// Optional order state tracker for explicit lifecycle tracking.
    /// When `Some`, every order transition (Open, PartiallyFilled, Filled,
    /// Cancelled, Rejected) is recorded. When `None`, zero overhead.
    pub(super) order_state_tracker: Option<super::order_state::OrderStateTracker>,

    /// Pluggable source of millisecond timestamps stamped on inbound
    /// orders, snapshots, and lifecycle transitions. Defaults to
    /// [`MonotonicClock`] (wall-clock); tests and sequencer replay can
    /// inject a [`super::clock::StubClock`] for byte-identical
    /// reproducibility. Not serialized — reconstructed on the restoring
    /// side like [`trade_listener`](Self::trade_listener).
    pub(super) clock: Arc<dyn Clock>,
}

/// A **lossy, inspection-only** serialization of the order book.
///
/// This `Serialize` impl is a human-readable debug/inspection dump, **not** a
/// persistence or round-trip path. It intentionally omits matching
/// configuration that [`OrderBook::create_snapshot_package`] preserves —
/// `stp_mode`, tick/lot/min/max order size, the engine sequence, the kill
/// switch, the risk config and the stop protection collar — and there is no
/// corresponding `Deserialize`, so it cannot reconstruct a book. The bids,
/// asks, and order-location maps use `BTreeMap` so the JSON key ordering is deterministic across process runs, and
/// the volatile best-bid/ask cache is not serialized.
///
/// For durable, reproducible persistence or replay, use
/// [`OrderBook::snapshot_to_json`] / [`OrderBook::create_snapshot_package`]
/// instead — those are the determinism-critical paths.
impl<T> Serialize for OrderBook<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        use std::collections::BTreeMap;
        use std::sync::atomic::Ordering;

        let mut state = serializer.serialize_struct("OrderBook", 9)?;

        // Serialize symbol
        state.serialize_field("symbol", &self.symbol)?;

        // Serialize bids as a BTreeMap<u128, PriceLevelSnapshot> so the key
        // ordering is deterministic (a HashMap would vary across runs).
        // `PriceLevel::snapshot` is fallible (pricelevel 0.10); a failed level
        // snapshot aborts serialization with a custom error instead of being
        // serialized as a `{"Ok": ..}` / `{"Err": ..}` wrapper.
        let bids: BTreeMap<u128, PriceLevelSnapshot> = self
            .bids
            .iter()
            .map(|entry| {
                entry
                    .value()
                    .snapshot()
                    .map(|snapshot| (*entry.key(), snapshot))
            })
            .collect::<Result<_, PriceLevelError>>()
            .map_err(serde::ser::Error::custom)?;
        state.serialize_field("bids", &bids)?;

        // Serialize asks as a BTreeMap<u128, PriceLevelSnapshot> (deterministic).
        // `PriceLevel::snapshot` is fallible (pricelevel 0.10); a failed level
        // snapshot aborts serialization with a custom error instead of being
        // serialized as a `{"Ok": ..}` / `{"Err": ..}` wrapper.
        let asks: BTreeMap<u128, PriceLevelSnapshot> = self
            .asks
            .iter()
            .map(|entry| {
                entry
                    .value()
                    .snapshot()
                    .map(|snapshot| (*entry.key(), snapshot))
            })
            .collect::<Result<_, PriceLevelError>>()
            .map_err(serde::ser::Error::custom)?;
        state.serialize_field("asks", &asks)?;

        // Serialize order_locations keyed by the order id's string form so the
        // map is both valid-JSON (string keys) and deterministically ordered.
        let order_locations: BTreeMap<String, (u128, Side)> = self
            .order_locations
            .iter()
            .map(|entry| (entry.key().to_string(), entry.value().price_side()))
            .collect();
        state.serialize_field("order_locations", &order_locations)?;

        // Serialize atomic values by loading them
        state.serialize_field("last_trade_price", &self.last_trade_price.load())?;
        state.serialize_field("has_traded", &self.has_traded.load(Ordering::Relaxed))?;
        state.serialize_field(
            "market_close_timestamp",
            &self.market_close_timestamp.load(Ordering::Relaxed),
        )?;
        state.serialize_field(
            "has_market_close",
            &self.has_market_close.load(Ordering::Relaxed),
        )?;

        // The volatile best-bid/ask cache is intentionally NOT serialized — it
        // is a recomputable optimization, not book state.

        // Serialize fee schedule
        state.serialize_field("fee_schedule", &self.fee_schedule)?;

        // Skip trade_listener (cannot be serialized) and transaction_id_generator, _phantom

        state.end()
    }
}

/// Engine-sequence minting, free of `T` bounds so the listener outbox
/// (`emission.rs`, #249) can stamp events from the submit-gate guard.
impl<T> OrderBook<T> {
    /// A panic unwound through a held submit gate (#249, #294): engage the
    /// kill switch and latch [`Self::submit_gate_poisoned`], logging once
    /// at `ERROR`. Called from [`SubmitGateGuard`]'s drop while the thread
    /// unwinds (either side, gate still held) and from
    /// [`Self::on_submit_gate_poisoned`]. Only atomics and one `tracing`
    /// event: no lock, no allocation of its own.
    #[cold]
    #[inline(never)]
    fn latch_submit_gate_unwind(&self, gate_side: &'static str) {
        // `engage_kill_switch`, inlined: this block is free of `T` bounds.
        self.kill_switch.store(true, Ordering::Relaxed);
        if !self.submit_gate_poisoned.swap(true, Ordering::Relaxed) {
            tracing::error!(
                symbol = %self.symbol,
                gate_side,
                "submit gate poisoned: code panicked mid-mutation while holding the submit gate; kill switch engaged, new flow and modifies are rejected until an operator releases it (cancels still run)"
            );
        }
    }

    /// Mint the next monotonic outbound sequence number.
    ///
    /// Called exactly once per outbound event (trade emission, price-level
    /// change emission). Internally a checked
    /// `AtomicU64::try_update(checked_add(1))` — strict total order across
    /// all events of this `OrderBook<T>` instance. Single source of truth
    /// for the minting contract; every emission path in the matching engine
    /// routes through this method so the counter cannot drift between
    /// sites.
    ///
    /// The contract is **per-instance**, not per-journal-stream: replay into
    /// a fresh book produces fresh seqs, not the original ones. Consumers
    /// that need to replay the exact original outbound stream should use
    /// the journal's `sequence_num` + `timestamp_ns` instead.
    ///
    /// # Exhaustion (#250)
    ///
    /// The counter holds the next value to mint and never wraps: the last
    /// mintable value is `u64::MAX - 1`, after which the counter rests at
    /// `u64::MAX` and every call fails. The engine's own emission paths
    /// then stop publishing `TradeResult` / `PriceLevelChangedEvent`
    /// events to the listeners (logged once at `ERROR`) rather than
    /// stamping a wrapped or repeated sequence; the book itself keeps
    /// working. Event stamping never affects a caller-owned result: an
    /// `add_order_with_result` / `*_with_committed` caller still receives
    /// its committed fills, with `engine_seq` set to
    /// [`UNSTAMPED_ENGINE_SEQ`]. Snapshot restore rejects a package whose
    /// `engine_seq` is already `u64::MAX`.
    ///
    /// # Errors
    ///
    /// [`OrderBookError::EngineSeqExhausted`] when the counter is at
    /// `u64::MAX` and cannot advance.
    #[inline]
    pub fn next_engine_seq(&self) -> Result<u64, OrderBookError> {
        self.engine_seq
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |seq| {
                seq.checked_add(1)
            })
            .map_err(engine_seq_exhausted)
    }

    /// Mint an `engine_seq` for an outbound event the engine is about to
    /// emit after a committed mutation (#250).
    ///
    /// Emission happens after the book has already changed, so an
    /// exhausted counter cannot be reported to the caller as a failure of
    /// the operation. Instead the event is suppressed: `None` is returned,
    /// the first exhaustion is logged at `ERROR` (latched, so a hot
    /// emission loop does not flood the log), and no event carrying a
    /// wrapped or repeated sequence is ever published.
    #[inline]
    pub(super) fn mint_event_seq(&self) -> Option<u64> {
        match self.next_engine_seq() {
            Ok(seq) => Some(seq),
            Err(_) => {
                self.latch_engine_seq_exhausted();
                None
            }
        }
    }

    /// Latch `engine_seq` exhaustion (#250): the first caller logs at
    /// `ERROR`; later calls are no-ops.
    #[cold]
    #[inline(never)]
    fn latch_engine_seq_exhausted(&self) {
        if !self.engine_seq_exhausted.swap(true, Ordering::Relaxed) {
            tracing::error!(
                symbol = %self.symbol,
                "engine_seq exhausted at u64::MAX: outbound trade and price-level events are no longer published"
            );
        }
    }

    /// Current value of the engine sequence counter without advancing.
    ///
    /// Used by snapshotting to capture the counter for later restore.
    #[inline]
    #[must_use]
    pub fn engine_seq(&self) -> u64 {
        self.engine_seq.load(Ordering::Acquire)
    }
}

/// Pending-stop fast path (#286), free of `T` bounds so the gate helpers can
/// read it.
impl<T> OrderBook<T> {
    /// `true` while at least one trailing stop is pending (#286). One
    /// relaxed load with `special_orders`, a constant `false` without it.
    ///
    /// The count only grows under the exclusive submit gate (admission and
    /// restore), so a caller holding the shared side that reads `false`
    /// keeps reading `false` until it releases the gate.
    #[inline]
    #[must_use]
    pub(super) fn has_pending_stops(&self) -> bool {
        #[cfg(feature = "special_orders")]
        {
            !self.pending_stops.is_empty()
        }
        #[cfg(not(feature = "special_orders"))]
        {
            false
        }
    }

    /// Whether admitting `order` adds a pending stop, which must happen
    /// under the exclusive submit gate (#286). Always `false` without
    /// `special_orders`, where a trailing stop is rejected untouched.
    #[inline]
    #[must_use]
    pub(super) fn admits_pending_stop<E>(order: &OrderType<E>) -> bool {
        cfg!(feature = "special_orders") && matches!(order, OrderType::TrailingStop { .. })
    }

    /// Whether `order_id` is a pending trailing stop (#286): the id is
    /// owned, like a resting order's location. `false` without
    /// `special_orders`.
    #[inline]
    #[must_use]
    pub(super) fn id_is_pending_stop(&self, order_id: Id) -> bool {
        #[cfg(feature = "special_orders")]
        {
            self.pending_stops.contains(order_id)
        }
        #[cfg(not(feature = "special_orders"))]
        {
            let _ = order_id;
            false
        }
    }
}

impl<T> OrderBook<T>
where
    T: Default + Clone + Send + Sync + 'static,
{
    /// Convert OrderType<()> to `OrderType<T>` for return values
    pub fn convert_from_unit_type(&self, order: &OrderType<()>) -> OrderType<T>
    where
        T: Default,
    {
        match order {
            OrderType::Standard {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::Standard {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: T::default(),
            },
            OrderType::IcebergOrder {
                id,
                price,
                visible_quantity,
                hidden_quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::IcebergOrder {
                id: *id,
                price: *price,
                visible_quantity: *visible_quantity,
                hidden_quantity: *hidden_quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: T::default(),
            },
            OrderType::PostOnly {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::PostOnly {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: T::default(),
            },
            OrderType::TrailingStop {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                trail_amount,
                last_reference_price,
                ..
            } => OrderType::TrailingStop {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                trail_amount: *trail_amount,
                last_reference_price: *last_reference_price,
                extra_fields: T::default(),
            },
            OrderType::PeggedOrder {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                reference_price_offset,
                reference_price_type,
                ..
            } => OrderType::PeggedOrder {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                reference_price_offset: *reference_price_offset,
                reference_price_type: *reference_price_type,
                extra_fields: T::default(),
            },
            OrderType::MarketToLimit {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::MarketToLimit {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: T::default(),
            },
            OrderType::ReserveOrder {
                id,
                price,
                visible_quantity,
                hidden_quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                replenish_threshold,
                replenish_amount,
                auto_replenish,
                ..
            } => OrderType::ReserveOrder {
                id: *id,
                price: *price,
                visible_quantity: *visible_quantity,
                hidden_quantity: *hidden_quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                replenish_threshold: *replenish_threshold,
                replenish_amount: *replenish_amount,
                auto_replenish: *auto_replenish,
                extra_fields: T::default(),
            },
        }
    }
    /// Create a new order book for the given symbol.
    ///
    /// The book is installed with a [`MonotonicClock`] (wall-clock
    /// milliseconds). Use [`Self::with_clock`] to inject a custom
    /// [`Clock`] implementation.
    ///
    /// The trade-ID namespace is derived without OS entropy and is unique
    /// per book instance, in this process and across restarts (UUIDv5 of
    /// symbol, process id, wall-clock nanoseconds and a process-wide
    /// counter, #265). Inject a fixed one with
    /// [`Self::set_trade_id_namespace`] for replay.
    pub fn new(symbol: &str) -> Self {
        Self::with_clock(symbol, Arc::new(MonotonicClock) as Arc<dyn Clock>)
    }

    /// Create a new order book for the given symbol with a
    /// caller-provided [`Clock`] implementation.
    ///
    /// Use this when byte-identical timestamp behaviour is required, e.g.
    /// for sequencer replay or deterministic tests — pass in a
    /// [`super::clock::StubClock`].
    ///
    /// The default trade-ID namespace is unique per book but not
    /// reproducible (see [`Self::new`]); use
    /// [`Self::with_clock_and_namespace`] for byte-identical trade IDs.
    pub fn with_clock(symbol: &str, clock: Arc<dyn Clock>) -> Self {
        // Unique per book without OS entropy (#265).
        let namespace = default_trade_id_namespace(symbol);

        Self {
            symbol: symbol.to_string(),
            bids: SkipMap::new(),
            asks: SkipMap::new(),
            order_locations: DashMap::new(),
            user_orders: DashMap::new(),
            transaction_id_generator: UuidGenerator::new(namespace),
            engine_seq: AtomicU64::new(0),
            kill_switch: AtomicBool::new(false),
            risk_state: RiskState::new(),
            last_trade_price: AtomicCell::new(0),
            has_traded: AtomicBool::new(false),
            strandable_makers_resting: AtomicUsize::new(0),
            match_aborts: AtomicU64::new(0),
            match_fold_failures: AtomicU64::new(0),
            trade_ids_exhausted: AtomicBool::new(false),
            engine_seq_exhausted: AtomicBool::new(false),
            submit_gate: std::sync::RwLock::new(()),
            submit_gate_poisoned: AtomicBool::new(false),
            outbox: super::emission::EventOutbox::default(),
            level_locks: std::array::from_fn(|_| std::sync::RwLock::new(())),
            #[cfg(test)]
            stp_interleave_hook: None,
            #[cfg(test)]
            level_interleave_hook: None,
            #[cfg(test)]
            fok_level_fault_hook: None,
            #[cfg(test)]
            cancel_fault_hook: None,
            #[cfg(test)]
            rest_fault_hook: None,
            #[cfg(test)]
            rest_interleave_hook: None,
            #[cfg(test)]
            rest_risk_fault_hook: None,
            #[cfg(test)]
            update_interleave_hook: None,
            #[cfg(all(test, feature = "special_orders"))]
            reprice_interleave_hook: None,
            #[cfg(test)]
            modify_interleave_hook: None,
            market_close_timestamp: AtomicU64::new(0),
            has_market_close: AtomicBool::new(false),
            cache: PriceLevelCache::new(),
            trade_listener: None,
            _phantom: PhantomData,
            price_level_changed_listener: None,
            #[cfg(feature = "special_orders")]
            special_order_tracker: SpecialOrderTracker::new(),
            #[cfg(feature = "special_orders")]
            pending_stops: super::stop_orders::PendingStops::new(),
            tick_size: None,
            lot_size: None,
            min_order_size: None,
            max_order_size: None,
            stp_mode: STPMode::None,
            fee_schedule: None,
            stop_protection: None,
            order_state_tracker: None,
            clock,
        }
    }

    /// Create a new order book with a caller-provided [`Clock`] and
    /// trade-ID namespace.
    ///
    /// Convenience constructor for the fully deterministic setup: the
    /// injected clock pins timestamps and the injected namespace pins the
    /// trade-ID stream (see [`Self::set_trade_id_namespace`]), so a live
    /// run and its replay over the same command stream produce
    /// byte-identical trades.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use orderbook_rs::OrderBook;
    /// use orderbook_rs::prelude::{Clock, StubClock};
    /// use uuid::Uuid;
    ///
    /// let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"VENUE/AAPL");
    /// let clock = Arc::new(StubClock::new()) as Arc<dyn Clock>;
    /// let book: OrderBook<()> = OrderBook::with_clock_and_namespace("AAPL", clock, namespace);
    /// assert_eq!(book.symbol(), "AAPL");
    /// ```
    pub fn with_clock_and_namespace(symbol: &str, clock: Arc<dyn Clock>, namespace: Uuid) -> Self {
        let mut book = Self::with_clock(symbol, clock);
        book.set_trade_id_namespace(namespace);
        book
    }

    /// Replace the clock used by this book.
    ///
    /// This takes `&mut self` and is intended for cold configuration
    /// paths — the production pattern is to set the clock once at
    /// construction via [`Self::with_clock`].
    pub fn set_clock(&mut self, clock: Arc<dyn Clock>) {
        self.clock = clock;
    }

    /// Replace the trade/transaction-ID namespace used by this book.
    ///
    /// Trade IDs are UUID v5 values derived from this namespace plus an
    /// atomic counter ([`pricelevel::UuidGenerator`]), so the namespace is
    /// the only entropy in the trade-ID stream: with an injected namespace
    /// (and an injected [`Clock`]) the same command stream produces
    /// byte-identical trade IDs across a live run and its replay. A
    /// deterministic choice such as UUID v5 of the symbol under a venue
    /// root namespace gives every book a stable, distinct stream.
    ///
    /// Call this **before any orders are submitted**: replacing the
    /// generator restarts its counter at 0, so a book that has already
    /// traded would re-issue the earliest IDs of the new namespace
    /// (duplicate trade IDs from a consumer's perspective). Like
    /// [`Self::set_clock`], this takes `&mut self` and is intended for
    /// cold configuration paths right after construction; it composes
    /// with every constructor without multiplying variants — or use
    /// [`Self::with_clock_and_namespace`] directly.
    pub fn set_trade_id_namespace(&mut self, namespace: Uuid) {
        self.transaction_id_generator = UuidGenerator::new(namespace);
        // #240: a fresh generator clears a latched exhaustion.
        *self.trade_ids_exhausted.get_mut() = false;
    }

    /// Number of matching sweeps this book aborted because a price level
    /// failed mid-sweep (#240), across every submission path. A growing
    /// value means the book is running into resource exhaustion (trade-id
    /// sequence, level counters, allocation); see
    /// [`OrderBookError::MatchAborted`].
    #[must_use]
    #[inline]
    pub fn match_aborts(&self) -> u64 {
        self.match_aborts.load(Ordering::Relaxed)
    }

    /// Number of price levels whose committed trades could not be folded
    /// into the taker's result (#240), plus committed sweeps whose
    /// `TradeResult` could not be priced with checked arithmetic (#244,
    /// unreachable for sweeps this book ran). These are the only paths on
    /// which the trade stream (listener / journal) can disagree with the
    /// book, risk and order-state streams; any non-zero value needs
    /// attention. See `doc/panic-boundaries.md`.
    #[must_use]
    #[inline]
    pub fn match_fold_failures(&self) -> u64 {
        self.match_fold_failures.load(Ordering::Relaxed)
    }

    /// `true` once the book has found its trade-id generator exhausted
    /// (#240). Latched: every crossing submit is then rejected untouched
    /// (`RejectReason::CapacityExceeded`) until the generator is replaced
    /// with [`Self::set_trade_id_namespace`]. The book does not engage the
    /// kill switch by itself.
    #[must_use]
    #[inline]
    pub fn trade_ids_exhausted(&self) -> bool {
        self.trade_ids_exhausted.load(Ordering::Relaxed)
    }

    /// `true` once an emission path has found `engine_seq` exhausted
    /// (#250). Latched: outbound `TradeResult` / `PriceLevelChangedEvent`
    /// emission is then suppressed (see [`Self::next_engine_seq`]) until a
    /// snapshot-package restore installs a counter that can advance.
    #[must_use]
    #[inline]
    pub fn engine_seq_exhausted(&self) -> bool {
        self.engine_seq_exhausted.load(Ordering::Relaxed)
    }

    /// Increment a diagnostic counter with checked arithmetic. At `u64::MAX`
    /// the counter stays put and the refusal is logged.
    #[inline]
    pub(super) fn bump_diagnostic_counter(counter: &AtomicU64, name: &'static str) {
        if counter
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .is_err()
        {
            tracing::warn!(
                counter = name,
                "diagnostic counter at u64::MAX; not incremented"
            );
        }
    }

    /// Latch trade-id exhaustion (#240): the first caller logs at `ERROR`
    /// and bumps the metric; later calls are no-ops.
    #[cold]
    #[inline(never)]
    pub(super) fn latch_trade_ids_exhausted(&self) {
        if !self.trade_ids_exhausted.swap(true, Ordering::Relaxed) {
            tracing::error!(
                symbol = %self.symbol,
                "trade-id generator exhausted: every crossing submit is rejected until the generator is replaced"
            );
            crate::orderbook::metrics::record_trade_ids_exhausted();
        }
    }

    /// Access the currently-installed clock.
    #[inline]
    #[must_use]
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Emit a [`PriceLevelChangedEvent`] to the installed listener, if any
    /// (#250, #249).
    ///
    /// Single emission helper for every price-level-change site. The event
    /// is buffered in the caller's emission scope and stamped with a fresh
    /// `engine_seq` when the mutation commits (under the submit gate, with
    /// [`Self::mint_event_seq`]'s exhaustion handling), then delivered after
    /// the gate is released (see `emission.rs`). Nothing is buffered or
    /// minted when no listener is installed, exactly as before.
    #[inline]
    pub(super) fn emit_price_level_changed(&self, side: Side, price: u128, quantity: u64) {
        if self.price_level_changed_listener.is_some() {
            self.defer_event(super::emission::PendingEvent::Level(
                PriceLevelChangedEvent {
                    side,
                    price,
                    quantity,
                    // Stamped at commit.
                    engine_seq: 0,
                },
            ));
        }
    }

    /// [`Self::emit_price_level_changed`] for a live level: the level's
    /// price and visible quantity are read only when a listener is
    /// installed, so the no-listener path does no extra work.
    #[inline]
    pub(super) fn emit_level_changed(&self, side: Side, level: &PriceLevel) {
        if self.price_level_changed_listener.is_some() {
            self.emit_price_level_changed(side, level.price(), level.visible_quantity());
        }
    }

    /// Refresh the operational depth gauges with the current count
    /// of distinct bid / ask price levels.
    ///
    /// Hooked from every structural mutation site so the published
    /// gauge tracks the book's true level count without affecting
    /// matching latency on the happy path.
    ///
    /// When the `metrics` feature is disabled this compiles to an
    /// empty function so the `bids.len()` / `asks.len()` reads are
    /// also elided — every caller is a true zero-cost no-op.
    #[cfg(feature = "metrics")]
    #[inline]
    pub(super) fn record_depth_metric(&self) {
        // A level count always fits `u64` on the supported targets; were it
        // not to, the gauge update is skipped rather than fed a clamped or
        // truncated value.
        if let (Ok(bid_levels), Ok(ask_levels)) = (
            u64::try_from(self.bids.len()),
            u64::try_from(self.asks.len()),
        ) {
            super::metrics::record_depth(bid_levels, ask_levels);
        }
    }

    /// No-op variant when the `metrics` feature is disabled.
    #[cfg(not(feature = "metrics"))]
    #[inline]
    pub(super) fn record_depth_metric(&self) {}

    /// Engage the kill switch. While engaged, every public `submit_*`,
    /// `add_order`, and non-cancel `update_order` call returns
    /// [`OrderBookError::KillSwitchActive`] before any matching, fee,
    /// or STP work happens. Cancel and mass-cancel paths are
    /// explicitly **not** gated so operators can drain the resting book.
    ///
    /// The flag persists across snapshot/restore. Idempotent — calling
    /// while already engaged is a no-op.
    ///
    /// Note: the low-level [`Self::match_market_order`] /
    /// [`Self::match_limit_order`] entry points are not gated.
    /// Production flow goes through the `submit_*` / `add_order` /
    /// `update_order` public surface.
    pub fn engage_kill_switch(&self) {
        self.kill_switch.store(true, Ordering::Relaxed);
    }

    /// Release the kill switch and resume accepting new flow.
    /// Idempotent.
    pub fn release_kill_switch(&self) {
        self.kill_switch.store(false, Ordering::Relaxed);
    }

    /// Current state of the kill switch.
    #[inline]
    #[must_use]
    pub fn is_kill_switch_engaged(&self) -> bool {
        self.kill_switch.load(Ordering::Relaxed)
    }

    /// Reject the current operation if the kill switch is engaged,
    /// recording an `OrderStatus::Rejected` transition for `order_id`
    /// when an order state tracker is configured.
    ///
    /// Use this from **new-flow** entry points (`submit_*`, `add_order`,
    /// `add_*_with_user`) where `order_id` identifies an order that has
    /// not yet entered the book — the `Rejected` lifecycle status is
    /// then accurate and consistent with its documented "rejected during
    /// validation, never entered" semantic.
    ///
    /// For modify / replace paths against orders already resting in the
    /// book, use [`Self::check_kill_switch`] instead — recording
    /// `Rejected` against a live order would corrupt the lifecycle
    /// state (the order remains active, the modification is what was
    /// rejected).
    ///
    /// Returns `Err(OrderBookError::KillSwitchActive)` when engaged so
    /// callers can early-return before any matching, fee, or STP work.
    /// Allocation-free on the happy path (no tracker write, no error
    /// construction); on the cold rejection path the tracker reason
    /// string is constructed via `to_string`.
    #[inline]
    pub(super) fn check_kill_switch_or_reject(&self, order_id: Id) -> Result<(), OrderBookError> {
        if self.is_kill_switch_engaged() {
            self.track_state(
                order_id,
                super::order_state::OrderStatus::Rejected {
                    reason: super::reject_reason::RejectReason::KillSwitchActive,
                },
            );
            return Err(OrderBookError::KillSwitchActive);
        }
        Ok(())
    }

    /// Reject the current operation if the kill switch is engaged
    /// **without** recording a tracker transition.
    ///
    /// Use this from modify / replace paths (`update_order` non-`Cancel`
    /// variants) where the existing order remains live — recording it
    /// as `Rejected` would conflict with the documented terminal-state
    /// semantic (`Rejected` means "never entered the book"). Only the
    /// modification is rejected; the underlying order stays as-is.
    #[inline]
    pub(super) fn check_kill_switch(&self) -> Result<(), OrderBookError> {
        if self.is_kill_switch_engaged() {
            return Err(OrderBookError::KillSwitchActive);
        }
        Ok(())
    }

    /// Install or replace the active risk configuration on this book.
    ///
    /// Counters and per-order risk state are preserved so that history
    /// from a previously-installed config remains consistent. Pass an
    /// empty [`RiskConfig`] (built via [`RiskConfig::new`]) to leave
    /// every gate disabled while keeping counters live for inspection.
    ///
    /// Risk gates run in the documented order
    /// `kill_switch → risk → STP → fees → match`, before any matching,
    /// fee, or STP work happens.
    pub fn set_risk_config(&mut self, config: RiskConfig) {
        self.risk_state.set_config(config);
    }

    /// Read-only access to the active risk configuration, if any.
    #[inline]
    #[must_use]
    pub fn risk_config(&self) -> Option<&RiskConfig> {
        self.risk_state.config()
    }

    /// Number of pre-trade risk accounting anomalies observed on this
    /// book (#243): a release larger than an account counter (a double
    /// release), a fill larger than a maker's tracked remainder, or a
    /// post-trade counter increment that would overflow. Each one is
    /// also logged with the order and account involved. Expected to be
    /// zero; see [`RiskState::accounting_anomalies`]. Not part of the
    /// snapshot.
    #[inline]
    #[must_use]
    pub fn risk_accounting_anomalies(&self) -> u64 {
        self.risk_state.accounting_anomalies()
    }

    /// Drop the active risk configuration. Counters and per-order risk
    /// state are retained so a subsequent [`Self::set_risk_config`]
    /// re-engages the gates without dropping history.
    pub fn disable_risk(&mut self) {
        self.risk_state.disable();
    }

    /// Resolve the reference price for the price-band check.
    ///
    /// `LastTrade` reads the atomic `last_trade_price` and returns
    /// `None` when no trade has executed yet on this book. `Mid`
    /// returns the integer midpoint of the best bid and ask when both
    /// are present; otherwise it falls back to `LastTrade`.
    /// `FixedPrice` always returns the operator-pinned value.
    #[inline]
    #[must_use]
    pub(super) fn resolve_reference_price(&self, source: ReferencePriceSource) -> Option<u128> {
        match source {
            ReferencePriceSource::LastTrade => self.last_trade_price(),
            ReferencePriceSource::Mid => {
                self.integer_mid_price().or_else(|| self.last_trade_price())
            }
            ReferencePriceSource::FixedPrice(p) => Some(p),
        }
    }

    /// Apply the pre-trade risk gates to a limit-order admission.
    ///
    /// Returns `Ok(())` immediately when no risk config is installed.
    /// Otherwise resolves the reference price (when the price band is
    /// configured) and delegates to
    /// [`RiskState::check_limit_admission`]. Allocation-free on the
    /// happy path; cold rejection allocates one error variant.
    #[inline]
    pub(super) fn check_risk_limit_admission(
        &self,
        account: pricelevel::Hash32,
        price: u128,
        quantity: u64,
    ) -> Result<(), OrderBookError> {
        let Some(cfg) = self.risk_state.config() else {
            return Ok(());
        };
        let reference = cfg
            .reference_price
            .and_then(|src| self.resolve_reference_price(src));
        self.risk_state
            .check_limit_admission(account, price, quantity, reference)
    }

    /// Take the **shared** side of the [`Self::level_locks`] stripe of
    /// `price`, held while admitting an order into that level (#247).
    ///
    /// Poisoning can only follow a panic that unwound while a stripe guard
    /// was held; the protected data is `()`, so recovery is always safe:
    /// the poison is logged at `ERROR` and the guard recovered. (The
    /// submit gate no longer recovers silently: since #249 its poisoning
    /// engages the kill switch, see [`Self::submit_gate_read`].) `None` is
    /// unreachable: the stripe index is
    /// `price % LEVEL_LOCK_STRIPES`, always in range.
    pub(super) fn lock_level(&self, price: u128) -> Option<std::sync::RwLockReadGuard<'_, ()>> {
        let lock = self.level_stripe(price)?;
        Some(lock.read().unwrap_or_else(|poisoned| {
            tracing::error!(
                "price level stripe lock poisoned by a prior panic; recovering read guard"
            );
            poisoned.into_inner()
        }))
    }

    /// Take the **exclusive** side of the [`Self::level_locks`] stripe of
    /// `price`, held while re-checking and unlinking an emptied level
    /// (#247). Same poisoning policy as [`Self::lock_level`].
    pub(super) fn lock_level_exclusive(
        &self,
        price: u128,
    ) -> Option<std::sync::RwLockWriteGuard<'_, ()>> {
        let lock = self.level_stripe(price)?;
        Some(lock.write().unwrap_or_else(|poisoned| {
            tracing::error!(
                "price level stripe lock poisoned by a prior panic; recovering write guard"
            );
            poisoned.into_inner()
        }))
    }

    /// The stripe of `price`: `price % LEVEL_LOCK_STRIPES`.
    fn level_stripe(&self, price: u128) -> Option<&std::sync::RwLock<()>> {
        let stripe = u128::try_from(LEVEL_LOCK_STRIPES)
            .ok()
            .and_then(|stripes| price.checked_rem(stripes))
            .and_then(|index| usize::try_from(index).ok())?;
        self.level_locks.get(stripe)
    }

    /// Removes the `side` level at `price` from the bid / ask map if, and
    /// only if, it is empty (#247). Returns whether a level was removed.
    ///
    /// This is the single place that unlinks a level while the book can be
    /// admitting concurrently: it holds the price's stripe exclusively, so
    /// no admission into the level can interleave (see [`Self::level_locks`]), and it
    /// re-reads the level **under** that stripe rather than trusting an
    /// earlier `order_count()` read or a remembered `Arc`: a level emptied
    /// earlier may have been refilled, or removed and re-created by someone
    /// else, and a non-empty level is never removed. The depth gauges are
    /// the caller's to refresh.
    pub(super) fn remove_level_if_empty(&self, side: Side, price: u128) -> bool {
        let levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        let _stripe = self.lock_level_exclusive(price);
        Self::remove_empty_level_locked(levels, price)
    }

    /// [`Self::remove_level_if_empty`] for a caller that already holds the
    /// stripe of `price` exclusively (the lock is not reentrant).
    fn remove_empty_level_locked(
        levels: &crossbeam_skiplist::SkipMap<u128, Arc<PriceLevel>>,
        price: u128,
    ) -> bool {
        let Some(entry) = levels.get(&price) else {
            return false;
        };
        if entry.value().order_count() != 0 {
            return false;
        }
        entry.remove()
    }

    /// Acquire the shared (read) side of the submit gate (#209).
    ///
    /// The returned guard also opens the call's listener emission scope
    /// when a listener is installed (#249): events the call produces are
    /// buffered, stamped when the guard is dropped (still under the gate),
    /// and delivered after the gate is released. See
    /// [`SubmitGateGuard`].
    ///
    /// # Poisoning (#249, #294)
    ///
    /// Listeners no longer run under the gate, so an unwind through a held
    /// gate can only mean engine code, or caller code the engine runs
    /// mid-mutation (a `Clock`, the metrics recorder, a `tracing`
    /// subscriber, `T::default()` / `T::clone()`), panicked, and the book
    /// may be inconsistent. [`SubmitGateGuard`]'s drop detects it with
    /// `std::thread::panicking()` on **both** sides (a
    /// `RwLockReadGuard` never poisons, so the shared side has no other
    /// signal): it engages the kill switch before the gate is released
    /// (every later new-flow call and modify returns
    /// [`OrderBookError::KillSwitchActive`]), latches
    /// [`Self::submit_gate_poisoned`] and logs once at `ERROR`. The
    /// exclusive side is also poisoned by std; the acquisition that finds
    /// it poisoned applies the same policy (idempotent), clears the poison
    /// and continues. Cancels and mass cancels keep working so the book
    /// can be drained, exactly as under an operator-engaged kill switch.
    pub(super) fn submit_gate_read(&self) -> SubmitGateGuard<'_, T> {
        let lock = self.submit_gate.read().unwrap_or_else(|poisoned| {
            self.on_submit_gate_poisoned();
            poisoned.into_inner()
        });
        SubmitGateGuard::new(self, GateLock::Read { _guard: lock })
    }

    /// Acquire the exclusive (write) side of the submit gate for a
    /// fill-or-kill submit (#209), an STP-relevant submit or
    /// matching-capable modify, the live snapshot restore commit (#225),
    /// and every mass cancel and expiry eviction (#248). See
    /// [`Self::submit_gate_read`] for the emission scope and the poisoning
    /// policy.
    pub(super) fn submit_gate_write(&self) -> SubmitGateGuard<'_, T> {
        SubmitGateGuard::new(
            self,
            GateLock::Write {
                _guard: self.submit_gate_write_raw(),
            },
        )
    }

    /// The bare exclusive lock, poison handled (see
    /// [`Self::submit_gate_read`]).
    fn submit_gate_write_raw(&self) -> std::sync::RwLockWriteGuard<'_, ()> {
        self.submit_gate.write().unwrap_or_else(|poisoned| {
            self.on_submit_gate_poisoned();
            poisoned.into_inner()
        })
    }

    /// A submit-gate acquisition found the gate poisoned (#249): engage the
    /// kill switch, latch the flag, log once, clear the poison.
    #[cold]
    #[inline(never)]
    fn on_submit_gate_poisoned(&self) {
        self.submit_gate.clear_poison();
        self.latch_submit_gate_unwind("exclusive");
    }

    /// `true` once code panicked while holding either side of the submit
    /// gate (#249, #294): engine code, or caller code the engine runs
    /// mid-mutation (a `Clock`, the metrics recorder, a `tracing`
    /// subscriber, `T::default()` / `T::clone()`). The book engaged its
    /// kill switch before releasing the gate, so
    /// new flow and modifies return [`OrderBookError::KillSwitchActive`]
    /// while cancels keep working. Latched for the life of the book (also
    /// after an operator releases the kill switch, which is the operator's
    /// statement that the book state is acceptable). Not part of the
    /// snapshot format; the kill switch it engaged is.
    #[must_use]
    #[inline]
    pub fn submit_gate_poisoned(&self) -> bool {
        self.submit_gate_poisoned.load(Ordering::Relaxed)
    }

    /// Acquire the submit gate in a mode that is **coherent with the
    /// strandable-maker count**, restarting in exclusive mode when the
    /// shared side turns out to be insufficient (#230).
    ///
    /// `wants_exclusive` is the caller's pre-acquisition decision, from
    /// [`submit_needs_exclusive_gate`](Self::submit_needs_exclusive_gate) or
    /// [`modify_needs_exclusive_gate`](Self::modify_needs_exclusive_gate).
    /// Both read [`strandable_makers_resting`](Self::strandable_makers_resting),
    /// which can change between that read and the acquisition, so this
    /// re-reads it once the shared side is held and, if the count is now
    /// non-zero, **drops the guard and takes the exclusive side afresh**.
    ///
    /// That is a restart, not a lock upgrade: `std::sync::RwLock` cannot
    /// upgrade, and nothing has been read or decided under the shared side
    /// at this point — the caller has not started its operation yet, so the
    /// whole operation runs under whichever side this returns.
    ///
    /// # The invariant this buys
    ///
    /// The count can only **increase** under the exclusive side: the two
    /// places that increment it are the admission path in `add_order_inner`
    /// and the snapshot-restore commit, and both hold the exclusive gate
    /// whenever a strandable maker is involved. So a caller that holds the
    /// shared side and has re-read the count as zero knows it will stay
    /// zero for as long as it holds that side. Conversely, in a book that
    /// *does* hold strandable makers every matching-capable submit and
    /// re-price runs exclusively, so:
    ///
    /// > Every **sweep** in a book holding a strandable maker runs
    /// > exclusively, so no cancel, mass cancel, admission or re-price can
    /// > land inside its capture window.
    ///
    /// Note the direction: the rule is enforced on the *sweep*, not on the
    /// cancels. Cancels and `UpdateQuantity` keep the shared side and never
    /// read the count; they are excluded from a sweep's window by that sweep
    /// holding the exclusive side, not by taking it themselves. Mass cancels
    /// and expiry eviction take the exclusive side for their own reasons
    /// (#248), which excludes them from any sweep as well. Every sweep entry point acquires through this helper —
    /// the submits, the modifies' re-add, and the match-only paths
    /// (`match_order`, `match_order_with_user`,
    /// `match_market_order_by_amount*`) — so an anonymous market sweep is
    /// covered too.
    ///
    /// That is what makes the sweep's capture attribution sound. Without it
    /// a sweep could capture maker X at a level, have X cancelled and its
    /// id reused by an unrelated `Standard` order admitted at the same
    /// level, and then report X's hidden quantity as discarded when it
    /// filled the impostor.
    ///
    /// Cost: a book holding strandable makers serializes its
    /// matching-capable submits and re-prices, exactly as an STP book has
    /// since #225. Cancels and `UpdateQuantity` keep the shared side — they
    /// simply cannot overlap an exclusive sweep in such a book. Books that hold none are unaffected: one relaxed load.
    /// # Why exclusive
    ///
    /// Both cases make a decision by reading book state and then act on
    /// that decision in a second, separate operation:
    ///
    /// - fill-or-kill checks multi-level feasibility, then sweeps;
    /// - an STP-active submit snapshots a price level's queue, decides the
    ///   [`STPAction`](super::stp::STPAction) for it, then fills it.
    ///
    /// Under the shared side another thread can mutate the level in
    /// between, so the decision is applied to state it was never taken on
    /// — a concurrent same-user admission could land behind an STP scan
    /// that found the level clean and then be filled by the very sweep the
    /// scan was protecting.
    ///
    /// # Boundary
    ///
    /// Every mutation performed through the `OrderBook` API either passes
    /// this gate or takes `&mut self` (the snapshot-package and JSON restore
    /// variants, exclusive by construction; #294 removed the public raw
    /// `place_order_in_book`, which bypassed it), so the exclusive holder
    /// observes a frozen book: its scan and
    /// its sweep see the same queue state, and a competing admission
    /// blocks here and runs against the post-decision state instead.
    ///
    /// Scope: the public API hands out no level handles — `get_bids` /
    /// `get_asks`, which cloned the live `Arc<PriceLevel>`s and let a caller
    /// mutate a level behind the gate, were removed in 0.13.0 (#228). Every
    /// level mutation therefore goes through `OrderBook` and past this gate.
    /// The read-only views (`create_snapshot`, the `LevelInfo` iterators,
    /// `get_orders_at_price`, `best_bid` / `best_ask`, …) hand out values,
    /// not handles.
    ///
    /// # Invariant: no nested acquisition
    ///
    /// `std::sync::RwLock` is not reentrant, so a nested acquisition may
    /// deadlock. Gate acquisition therefore lives ONLY in the public
    /// mutating entry points, which call ungated inner variants for any
    /// internal composition (`add_order_inner`,
    /// `cancel_order_with_reason`, `match_order_with_user_outcome`). User
    /// callbacks are not affected (#249): [`TradeListener`],
    /// [`PriceLevelChangedListener`] and
    /// [`OrderStateListener`](super::order_state::OrderStateListener) run
    /// after the guard released the gate, so they may re-enter the book.
    pub(super) fn acquire_coherent_submit_gate(
        &self,
        wants_exclusive: bool,
    ) -> SubmitGateGuard<'_, T> {
        self.acquire_submit_gate_for(wants_exclusive, true)
    }

    /// [`Self::acquire_coherent_submit_gate`] for a call that may or may
    /// not trade (#286).
    ///
    /// A call that can trade (`can_trade`) must run exclusively while a
    /// trailing stop is pending: its trades are evaluated against the
    /// pending stops before it returns, and every mutation of the stop
    /// store runs under the exclusive side. A call that cannot trade (a
    /// post-only add, a quantity update or cancel of a resting order) keeps
    /// the shared side. Like the strandable-maker count, the pending count
    /// only grows under the exclusive side, so the re-check after the
    /// shared acquisition is final.
    pub(super) fn acquire_submit_gate_for(
        &self,
        wants_exclusive: bool,
        can_trade: bool,
    ) -> SubmitGateGuard<'_, T> {
        if wants_exclusive {
            return self.submit_gate_write();
        }
        let shared = self.submit_gate.read().unwrap_or_else(|poisoned| {
            self.on_submit_gate_poisoned();
            poisoned.into_inner()
        });
        if self.strandable_makers_resting.load(Ordering::Relaxed) == 0
            && !(can_trade && self.has_pending_stops())
        {
            return SubmitGateGuard::new(self, GateLock::Read { _guard: shared });
        }
        // A strandable maker or, for a call that can trade, a pending stop
        // (#286) was admitted between the caller's decision and this
        // acquisition. Release and start over on the exclusive side.
        drop(shared);
        self.submit_gate_write()
    }

    /// Acquire the submit gate for a call that cannot trade but may target
    /// `order_id` (#286): a single-order cancel (`coherent == false`: the
    /// plain shared side, cancels ignore the strandable-maker count), or a
    /// quantity update / cancel through `update_order` (`coherent == true`:
    /// [`Self::acquire_submit_gate_for`] with `wants_exclusive`, that
    /// call's own decision).
    ///
    /// Shared, unless `order_id` is a pending trailing stop: every mutation
    /// of the stop store runs under the exclusive side. The target is
    /// re-checked once the shared side is held (the stop set only changes
    /// under the exclusive side, so the answer is final); a stop target
    /// releases the shared side and takes the exclusive one. A call on any
    /// other id runs concurrently, as it does on a book without stops.
    pub(super) fn acquire_gate_for_target(
        &self,
        wants_exclusive: bool,
        order_id: Id,
        coherent: bool,
    ) -> SubmitGateGuard<'_, T> {
        if wants_exclusive || self.id_is_pending_stop(order_id) {
            return self.submit_gate_write();
        }
        let gate = if coherent {
            self.acquire_submit_gate_for(false, false)
        } else {
            self.submit_gate_read()
        };
        if !gate.is_shared() || !self.id_is_pending_stop(order_id) {
            return gate;
        }
        // A stop with this id was admitted between the check and the
        // acquisition. The shared guard's emission scope is still empty:
        // dropping it commits nothing.
        drop(gate);
        self.submit_gate_write()
    }

    /// Decide the submit gate mode for an incoming order (#209 / #225 / #230).
    ///
    /// Returns `true` — exclusive — when any of:
    ///
    /// - the order is fill-or-kill, whose feasibility check and sweep must
    ///   not interleave with any other mutation (#209);
    /// - self-trade prevention is engaged on this book, the taker carries a
    ///   real identity, and the taker can actually take liquidity, so the
    ///   per-level STP scan and the fill it authorises must observe the same
    ///   queue state (#225); or
    /// - the order is a **strandable maker** — a
    ///   `ReserveOrder { auto_replenish: false, .. }` with hidden quantity —
    ///   in **every** [`STPMode`], including
    ///   [`None`](super::stp::STPMode::None) (#230, see below); or
    /// - a trailing stop is pending (#286) and the submit can trade, so the
    ///   stops are evaluated after it with no concurrent mutation. A
    ///   post-only submit never trades and stays on the **shared** side even
    ///   while stops are pending (it cannot move the last trade price, and
    ///   the stop store only changes under the exclusive side).
    ///
    /// # Why a strandable maker is admitted exclusively
    ///
    /// A sweep decides once, up front, whether to run its per-level
    /// strandable-maker capture, by reading
    /// [`strandable_makers_resting`](Self::strandable_makers_resting). If a
    /// strandable maker could be admitted while that sweep is in flight, the
    /// sweep could consume and remove a maker it never captured, dropping
    /// the hidden tranche with no report and no counter movement:
    ///
    /// ```text
    /// asks 1@100, 1@101; a buy of 3@101 starts and reads the count as 0
    /// ...the 100 level fills...
    /// another thread admits a reserve {visible 1, hidden 20, auto off} @101
    /// ...the sweep reaches 101, consumes and removes it, reports nothing
    /// ```
    ///
    /// Admitting such a maker on the exclusive side closes that window: the
    /// admission blocks until every in-flight sweep has released the shared
    /// side, so the once-per-sweep read and the per-level captures observe
    /// the same set of strandable makers. The count can still *fall* during
    /// a sweep (a concurrent cancel), which only leaves the sweep scanning
    /// for a maker that is already gone — it captures nothing and reports
    /// nothing, which is correct.
    ///
    /// Cost: submitting such a reserve serializes against every other
    /// submit, cancel, modify and sweep on the book, exactly as a
    /// fill-or-kill or an STP-relevant submit already did. Nothing else
    /// changes mode — the shape is rare, and every other submit on an
    /// `STPMode::None` book keeps the shared, fully concurrent path.
    ///
    /// # Why post-only is excluded
    ///
    /// A post-only taker never runs the STP scan: the post-only branch in
    /// `match_order_with_user_outcome` resolves each crossing level with
    /// `break` (rejected) or `continue` (walk on) *before* the STP block is
    /// reached, so it has no check-then-act window of its own. Its
    /// reject-or-rest decision is `pricelevel`'s structural probe, which is
    /// linearized on the level itself and never trades (#209), and STP is
    /// deliberately never consulted for it. Post-only is excluded from an
    /// *identified* taker's window by that taker holding the write side, not
    /// by taking the write side itself — which is exactly what T1 of the
    /// #225 suite exercises, with the post-only as the blocked competitor.
    ///
    /// # Why anonymous takers stay shared
    ///
    /// `match_order_with_user_outcome` skips the STP scan entirely for a
    /// zero `taker_user_id` (see
    /// [`check_stp_at_level`](super::stp::check_stp_at_level)), so there is
    /// no window to protect. On an STP-enabled book that shared path is
    /// reachable **only** through the match-only entry points —
    /// [`match_order_with_user`](Self::match_order_with_user),
    /// [`match_market_order_with_user`](Self::match_market_order_with_user)
    /// and
    /// [`match_market_order_by_amount_with_user`](Self::match_market_order_by_amount_with_user)
    /// — because `validate_order_shape` rejects an `add_order` carrying
    /// `Hash32::zero()` with [`OrderBookError::MissingUserId`]. An anonymous
    /// taker therefore does not buy back concurrency for identified flow: it
    /// still blocks behind, and is blocked by, every identified submit.
    ///
    /// Books left on [`STPMode::None`] keep the shared, fully concurrent
    /// submit path for every order.
    #[inline]
    #[must_use]
    pub(super) fn submit_needs_exclusive_gate(
        &self,
        is_fill_or_kill: bool,
        taker_user_id: Hash32,
        is_post_only: bool,
        rests_strandable_maker: bool,
    ) -> bool {
        if is_fill_or_kill || rests_strandable_maker {
            return true;
        }
        // A post-only submit never takes liquidity, so it can neither run
        // the STP scan nor consume a strandable maker: it stays shared under
        // both rules.
        if is_post_only {
            return false;
        }
        (self.stp_mode.is_enabled() && taker_user_id != Hash32::zero())
            // #230: any submit that can match must not interleave with the
            // strandable-maker capture of a concurrent sweep, nor have its
            // own capture invalidated by a concurrent cancel + id reuse.
            || self.strandable_makers_resting.load(Ordering::Relaxed) > 0
            // #286: while a trailing stop is pending, every mutator runs
            // exclusively, so the stops are evaluated (and their market
            // orders executed) with no concurrent mutation. A post-only
            // submit returns early above and stays shared: it cannot trade,
            // so it cannot move the last trade price.
            || self.has_pending_stops()
    }

    /// Decide the submit gate mode for an [`OrderUpdate`] (#225 / #230).
    ///
    /// Returns `true` — exclusive — for the cancel-then-add forms whose
    /// re-add can match against the book (`UpdatePrice`,
    /// `UpdatePriceAndQuantity`, `Replace`) when either:
    ///
    /// - STP is engaged, because those re-adds run the same per-level STP
    ///   scan as a fresh submit and carry the same check-then-act window
    ///   (#225); or
    /// - the book holds **any** strandable maker
    ///   (`strandable_makers_resting > 0`), in every `STPMode` (#230). The
    ///   re-add is a matching-capable submit, so it must not interleave with
    ///   a concurrent sweep's capture; and holding the exclusive side across
    ///   the *whole* modify — lookup, validation, cancel and re-add — also
    ///   makes [`check_modify_reserve_residual`](Self::check_modify_reserve_residual)'s
    ///   crossable-depth dry run exact, since no concurrent mutation can
    ///   move the opposite side between the estimate and the re-add's sweep.
    ///
    /// `UpdateQuantity` either adjusts a resting order in place or, on a
    /// zero `new_quantity`, removes it outright (#223), and `Cancel` only
    /// removes one. Neither variant ever re-adds an order, so neither can
    /// match and neither needs an exclusive window: both keep the shared
    /// side under either rule.
    ///
    /// # Why the count, not a lookup of the order being modified
    ///
    /// Deciding from `self.get_order(order_id)` would read book state
    /// *outside* the gate, where it can go stale before the acquisition:
    /// the order could be cancelled, or its id reused, between the lookup
    /// and the gate. The count is the safe pre-acquisition read because it
    /// is monotone under the shared side —
    /// [`acquire_coherent_submit_gate`](Self::acquire_coherent_submit_gate)
    /// re-checks it once the gate is held and restarts exclusively if it
    /// grew. A resting strandable reserve implies a count of at least one,
    /// so the coarser rule subsumes the per-order one and additionally
    /// covers the id-reuse case, where the order being modified is not
    /// itself strandable but a concurrent sweep's capture is.
    #[inline]
    #[must_use]
    pub(super) fn modify_needs_exclusive_gate(&self, update: &OrderUpdate) -> bool {
        // #286: while a trailing stop is pending, the variants that can
        // trade (a re-add evaluates the stops before it returns) run
        // exclusively; `UpdateQuantity` / `Cancel` only when they target a
        // pending stop (re-checked under the gate, see
        // `acquire_gate_for_target`).
        if self.has_pending_stops() {
            return match update {
                OrderUpdate::UpdatePrice { .. }
                | OrderUpdate::UpdatePriceAndQuantity { .. }
                | OrderUpdate::Replace { .. } => true,
                OrderUpdate::UpdateQuantity { order_id, .. } | OrderUpdate::Cancel { order_id } => {
                    self.id_is_pending_stop(*order_id)
                }
            };
        }
        // Exhaustive on purpose: a new `OrderUpdate` variant must force an
        // explicit decision here rather than silently inherit the shared
        // side. `OrderUpdate` is not `#[non_exhaustive]` in pricelevel
        // 0.9.1, so no wildcard arm is needed.
        match update {
            OrderUpdate::UpdatePrice { .. }
            | OrderUpdate::UpdatePriceAndQuantity { .. }
            | OrderUpdate::Replace { .. } => {
                self.stp_mode.is_enabled()
                    || self.strandable_makers_resting.load(Ordering::Relaxed) > 0
            }
            OrderUpdate::UpdateQuantity { .. } | OrderUpdate::Cancel { .. } => false,
        }
    }

    /// Apply the pre-trade risk gates to an in-place **modify** of a
    /// resting order (`UpdatePrice` / `UpdatePriceAndQuantity` /
    /// `Replace`).
    ///
    /// Returns `Ok(())` immediately when no risk config is installed.
    /// Otherwise resolves the reference price (the same way as
    /// [`Self::check_risk_limit_admission`]) and delegates to
    /// [`RiskState::check_modify_admission`], which checks the price band
    /// on `new_price` and the notional *projected* by swapping the
    /// original order's `old_price * old_qty` contribution for
    /// `new_price * new_qty` — but does not re-check the open-order count,
    /// because a modify keeps it unchanged. Allocation-free on the happy
    /// path; cold rejection allocates one error variant.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_risk_modify_admission(
        &self,
        order_id: pricelevel::Id,
        account: pricelevel::Hash32,
        new_price: u128,
        new_qty: u64,
    ) -> Result<(), OrderBookError> {
        let Some(cfg) = self.risk_state.config() else {
            return Ok(());
        };
        let reference = cfg
            .reference_price
            .and_then(|src| self.resolve_reference_price(src));
        self.risk_state
            .check_modify_admission(order_id, account, new_price, new_qty, reference)
    }

    /// Create a new order book for the given symbol with tick size validation.
    ///
    /// Orders added to this book must have prices that are exact multiples
    /// of `tick_size`. For example, with `tick_size = 100`, prices 100, 200,
    /// 300 are valid but 150 is rejected.
    ///
    /// # Arguments
    /// - `symbol`: The trading symbol for this order book
    /// - `tick_size`: Minimum price increment. Must be > 0
    ///
    /// # Returns
    /// A new `OrderBook` instance with tick size validation enabled
    pub fn with_tick_size(symbol: &str, tick_size: u128) -> Self {
        let mut book = Self::new(symbol);
        book.tick_size = Some(tick_size);
        book
    }

    /// Create a new order book for the given symbol with lot size validation.
    ///
    /// Orders added to this book must have quantities that are exact multiples
    /// of `lot_size`. For the two-tranche kinds (iceberg and reserve) both the
    /// visible and the hidden quantity are validated individually, and a
    /// reserve is additionally validated on the quantity its replenishment
    /// would transfer into the visible tranche — see `validate_order_shape`
    /// for the per-kind rules.
    ///
    /// The default transfer is capped by the hidden tranche: a reserve order
    /// that leaves `replenish_amount` unset while `auto_replenish` is on is
    /// validated on `min(DEFAULT_RESERVE_REPLENISH_AMOUNT, hidden)`, where
    /// `pricelevel`'s
    /// [`DEFAULT_RESERVE_REPLENISH_AMOUNT`](pricelevel::DEFAULT_RESERVE_REPLENISH_AMOUNT)
    /// is 80 quantity units. While `hidden < 80` the transfer is the already
    /// lot-aligned hidden tranche itself and the order is admitted; once
    /// `hidden >= 80` the transfer is exactly 80, so on a lot size that does
    /// not divide 80 (100, 25, 30, 60, 3, …) such an order is rejected with
    /// `InvalidLotSize { quantity: 80, .. }`. On a lot-25 book, 25 visible /
    /// 50 hidden is admitted (`min(80, 50) = 50`) and 25 / 100 is rejected.
    /// Set an explicit lot-aligned `replenish_amount` when a larger hidden
    /// tranche is needed on such a book.
    ///
    /// # Arguments
    /// - `symbol`: The trading symbol for this order book
    /// - `lot_size`: Minimum quantity increment. Must be > 0
    ///
    /// # Returns
    /// A new `OrderBook` instance with lot size validation enabled
    pub fn with_lot_size(symbol: &str, lot_size: u64) -> Self {
        let mut book = Self::new(symbol);
        book.lot_size = Some(lot_size);
        book
    }

    /// Create a new order book for the given symbol with a trade listener
    pub fn with_trade_listener(symbol: &str, trade_listener: TradeListener) -> Self {
        let namespace = default_trade_id_namespace(symbol);

        Self {
            symbol: symbol.to_string(),
            bids: SkipMap::new(),
            asks: SkipMap::new(),
            order_locations: DashMap::new(),
            user_orders: DashMap::new(),
            transaction_id_generator: UuidGenerator::new(namespace),
            engine_seq: AtomicU64::new(0),
            kill_switch: AtomicBool::new(false),
            risk_state: RiskState::new(),
            last_trade_price: AtomicCell::new(0),
            has_traded: AtomicBool::new(false),
            strandable_makers_resting: AtomicUsize::new(0),
            match_aborts: AtomicU64::new(0),
            match_fold_failures: AtomicU64::new(0),
            trade_ids_exhausted: AtomicBool::new(false),
            engine_seq_exhausted: AtomicBool::new(false),
            submit_gate: std::sync::RwLock::new(()),
            submit_gate_poisoned: AtomicBool::new(false),
            outbox: super::emission::EventOutbox::default(),
            level_locks: std::array::from_fn(|_| std::sync::RwLock::new(())),
            #[cfg(test)]
            stp_interleave_hook: None,
            #[cfg(test)]
            level_interleave_hook: None,
            #[cfg(test)]
            fok_level_fault_hook: None,
            #[cfg(test)]
            cancel_fault_hook: None,
            #[cfg(test)]
            rest_fault_hook: None,
            #[cfg(test)]
            rest_interleave_hook: None,
            #[cfg(test)]
            rest_risk_fault_hook: None,
            #[cfg(test)]
            update_interleave_hook: None,
            #[cfg(all(test, feature = "special_orders"))]
            reprice_interleave_hook: None,
            #[cfg(test)]
            modify_interleave_hook: None,
            market_close_timestamp: AtomicU64::new(0),
            has_market_close: AtomicBool::new(false),
            cache: PriceLevelCache::new(),
            trade_listener: Some(trade_listener),
            _phantom: PhantomData,
            price_level_changed_listener: None,
            #[cfg(feature = "special_orders")]
            special_order_tracker: SpecialOrderTracker::new(),
            #[cfg(feature = "special_orders")]
            pending_stops: super::stop_orders::PendingStops::new(),
            tick_size: None,
            lot_size: None,
            min_order_size: None,
            max_order_size: None,
            stp_mode: STPMode::None,
            fee_schedule: None,
            stop_protection: None,
            order_state_tracker: None,
            clock: Arc::new(MonotonicClock) as Arc<dyn Clock>,
        }
    }

    /// Creates a new order book with both a trade listener and a price level change listener.
    ///
    /// # Arguments
    /// - `symbol`: The trading symbol for this order book
    /// - `trade_listener`: Callback invoked when trades are executed
    /// - `book_changed_listener`: Callback invoked when price levels change
    ///
    /// # Returns
    /// A new `OrderBook` instance with both listeners configured
    pub fn with_trade_and_price_level_listener(
        symbol: &str,
        trade_listener: TradeListener,
        book_changed_listener: PriceLevelChangedListener,
    ) -> Self {
        let namespace = default_trade_id_namespace(symbol);

        Self {
            symbol: symbol.to_string(),
            bids: SkipMap::new(),
            asks: SkipMap::new(),
            order_locations: DashMap::new(),
            user_orders: DashMap::new(),
            transaction_id_generator: UuidGenerator::new(namespace),
            engine_seq: AtomicU64::new(0),
            kill_switch: AtomicBool::new(false),
            risk_state: RiskState::new(),
            last_trade_price: AtomicCell::new(0),
            has_traded: AtomicBool::new(false),
            strandable_makers_resting: AtomicUsize::new(0),
            match_aborts: AtomicU64::new(0),
            match_fold_failures: AtomicU64::new(0),
            trade_ids_exhausted: AtomicBool::new(false),
            engine_seq_exhausted: AtomicBool::new(false),
            submit_gate: std::sync::RwLock::new(()),
            submit_gate_poisoned: AtomicBool::new(false),
            outbox: super::emission::EventOutbox::default(),
            level_locks: std::array::from_fn(|_| std::sync::RwLock::new(())),
            #[cfg(test)]
            stp_interleave_hook: None,
            #[cfg(test)]
            level_interleave_hook: None,
            #[cfg(test)]
            fok_level_fault_hook: None,
            #[cfg(test)]
            cancel_fault_hook: None,
            #[cfg(test)]
            rest_fault_hook: None,
            #[cfg(test)]
            rest_interleave_hook: None,
            #[cfg(test)]
            rest_risk_fault_hook: None,
            #[cfg(test)]
            update_interleave_hook: None,
            #[cfg(all(test, feature = "special_orders"))]
            reprice_interleave_hook: None,
            #[cfg(test)]
            modify_interleave_hook: None,
            market_close_timestamp: AtomicU64::new(0),
            has_market_close: AtomicBool::new(false),
            cache: PriceLevelCache::new(),
            trade_listener: Some(trade_listener),
            _phantom: PhantomData,
            price_level_changed_listener: Some(book_changed_listener),
            #[cfg(feature = "special_orders")]
            special_order_tracker: SpecialOrderTracker::new(),
            #[cfg(feature = "special_orders")]
            pending_stops: super::stop_orders::PendingStops::new(),
            tick_size: None,
            lot_size: None,
            min_order_size: None,
            max_order_size: None,
            stp_mode: STPMode::None,
            fee_schedule: None,
            stop_protection: None,
            order_state_tracker: None,
            clock: Arc::new(MonotonicClock) as Arc<dyn Clock>,
        }
    }

    /// Set a trade listener for this order book
    pub fn set_trade_listener(&mut self, trade_listener: TradeListener) {
        self.trade_listener = Some(trade_listener);
    }

    /// Remove the trade listener from this order book
    pub fn remove_trade_listener(&mut self) {
        self.trade_listener = None;
    }

    /// set price level listener for this order book
    pub fn set_price_level_listener(&mut self, listener: PriceLevelChangedListener) {
        self.price_level_changed_listener = Some(listener);
    }

    /// remove price level listener for this order book
    pub fn remove_price_level_listener(&mut self) {
        self.price_level_changed_listener = None;
    }

    /// Set the fee schedule for this order book
    ///
    /// The fee schedule defines maker and taker fees in basis points.
    /// When set, fees will be calculated during trade execution.
    /// Set to None to disable fees.
    ///
    /// # Arguments
    ///
    /// * `fee_schedule` - The fee schedule to use, or None to disable fees
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::{OrderBook, FeeSchedule};
    ///
    /// let mut book = OrderBook::<()>::new("BTC/USD");
    ///
    /// // Set standard fees: 2 bps maker rebate, 5 bps taker fee
    /// let schedule = FeeSchedule::new(-2, 5);
    /// book.set_fee_schedule(Some(schedule));
    ///
    /// // Disable fees
    /// book.set_fee_schedule(None);
    /// ```
    pub fn set_fee_schedule(&mut self, fee_schedule: Option<FeeSchedule>) {
        self.fee_schedule = fee_schedule;
    }

    /// Get the current fee schedule for this order book
    ///
    /// # Returns
    ///
    /// The current fee schedule, or None if no fees are configured
    #[must_use]
    pub fn fee_schedule(&self) -> Option<FeeSchedule> {
        self.fee_schedule
    }

    /// Install, replace or clear (`None`) the protection collar for elected
    /// stop orders (#302).
    ///
    /// With a collar, an elected trailing stop executes as an
    /// immediate-or-cancel **limit** order at its stop price (the trailed
    /// stop price at election) moved by the collar against it: a sell stop
    /// at `stop - collar`, a buy stop at `stop + collar` (see
    /// [`StopProtection::limit_price`]). It trades only within that band and
    /// whatever does not fill is cancelled, not rested (unlike CME
    /// protection points): the stop ends `Cancelled { StopProtectionBand }`
    /// with what it filled when liquidity remained beyond the limit, or
    /// `Cancelled { InsufficientLiquidity }` when the side ran out within
    /// the band. A stop whose band is exhausted leaves its position
    /// unprotected. `Triggered { limit_price }` records the limit. Without
    /// one (the default) it executes as an unpriced immediate-or-cancel
    /// market order, the 0.14 behaviour.
    ///
    /// Applies to stops elected after the call, including stops already
    /// pending. Trailing stops need the `special_orders` feature; without
    /// it the collar is kept (and snapshotted) but has nothing to act on.
    ///
    /// With a tick size configured the collar must be a multiple of it, so
    /// the collar limit of a tick-aligned stop price is tick-aligned. Like
    /// the book's other shape rules, a later [`Self::set_tick_size`] does
    /// not re-validate it: the collar keeps bounding execution exactly
    /// (only levels at or inside the band trade), it is simply no longer a
    /// whole number of ticks.
    ///
    /// # Errors
    ///
    /// [`OrderBookError::InvalidTickSize`] (carrying the collar as `price`)
    /// when the collar is not a multiple of the book's tick size; the
    /// previous protection is kept.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::{OrderBook, OrderBookError, StopProtection};
    ///
    /// let mut book = OrderBook::<()>::with_tick_size("BTC/USD", 5);
    /// book.set_stop_protection(Some(StopProtection::try_new(25)?))?;
    /// assert_eq!(book.stop_protection().map(StopProtection::collar), Some(25));
    ///
    /// // 12 is not a multiple of the tick size.
    /// assert!(book.set_stop_protection(Some(StopProtection::try_new(12)?)).is_err());
    /// assert_eq!(book.stop_protection().map(StopProtection::collar), Some(25));
    ///
    /// // Back to the unprotected market child.
    /// book.set_stop_protection(None)?;
    /// # Ok::<(), OrderBookError>(())
    /// ```
    pub fn set_stop_protection(
        &mut self,
        protection: Option<StopProtection>,
    ) -> Result<(), OrderBookError> {
        if let Some(protection) = protection {
            protection.check_tick_size(self.tick_size)?;
        }
        self.stop_protection = protection;
        Ok(())
    }

    /// Install or clear the protection collar without the tick-size check
    /// (#302): the restore and replay paths reproduce a source book as it
    /// was, and a source book may hold a collar admitted under a previous
    /// tick size (see [`Self::set_stop_protection`]).
    #[inline]
    pub(crate) fn set_stop_protection_unchecked(&mut self, protection: Option<StopProtection>) {
        self.stop_protection = protection;
    }

    /// The protection collar for elected stop orders, if any (#302).
    ///
    /// `None` (the default) means an elected stop executes as an unpriced
    /// immediate-or-cancel market order.
    #[inline]
    #[must_use]
    pub fn stop_protection(&self) -> Option<StopProtection> {
        self.stop_protection
    }

    /// Set the minimum price increment for orders.
    ///
    /// When set, order prices must be exact multiples of this value.
    /// For example, with `tick_size = 100`, prices 100, 200, 300 are valid
    /// but 150 is rejected with `OrderBookError::InvalidTickSize`.
    ///
    /// # Arguments
    /// - `tick_size`: Minimum price increment. Must be > 0
    pub fn set_tick_size(&mut self, tick_size: u128) {
        self.tick_size = Some(tick_size);
    }

    /// Set or clear the minimum price increment from an [`Option`].
    ///
    /// `Some(t)` enables tick validation at `t`; `None` disables it. This is
    /// the setter the sequencer [`ReplayEngine`](crate::ReplayEngine) uses to
    /// inject `tick_size` into a fresh book before replay so that a journal
    /// produced by a tick-constrained book reconstructs to the same structure.
    ///
    /// Intended for replay / restore on a **fresh** book. Changing the tick
    /// size on a book that already holds resting orders does not re-validate
    /// or re-price existing levels — that is the caller's responsibility.
    ///
    /// # Arguments
    /// - `tick_size`: `Some(increment)` to enable (increment must be > 0), or
    ///   `None` to disable validation.
    #[inline]
    pub fn set_tick_size_opt(&mut self, tick_size: Option<u128>) {
        self.tick_size = tick_size;
    }

    /// Returns the configured tick size, if any.
    ///
    /// `None` means tick size validation is disabled (all prices accepted).
    #[must_use]
    pub fn tick_size(&self) -> Option<u128> {
        self.tick_size
    }

    /// Set the minimum quantity increment for orders.
    ///
    /// When set, order quantities must be exact multiples of this value.
    /// For the two-tranche kinds (iceberg and reserve) both the visible and
    /// the hidden quantity are validated individually, and a reserve is
    /// additionally validated on the quantity its replenishment would
    /// transfer into the visible tranche — see `validate_order_shape` for
    /// the per-kind rules. Rejection returns
    /// `OrderBookError::InvalidLotSize`.
    ///
    /// The default transfer is capped by the hidden tranche: a reserve order
    /// that leaves `replenish_amount` unset while `auto_replenish` is on is
    /// validated on `min(DEFAULT_RESERVE_REPLENISH_AMOUNT, hidden)`, where
    /// `pricelevel`'s
    /// [`DEFAULT_RESERVE_REPLENISH_AMOUNT`](pricelevel::DEFAULT_RESERVE_REPLENISH_AMOUNT)
    /// is 80 quantity units. While `hidden < 80` the transfer is the already
    /// lot-aligned hidden tranche itself and the order is admitted; once
    /// `hidden >= 80` the transfer is exactly 80, so on a lot size that does
    /// not divide 80 (100, 25, 30, 60, 3, …) such an order is rejected with
    /// `InvalidLotSize { quantity: 80, .. }`. On a lot-25 book, 25 visible /
    /// 50 hidden is admitted (`min(80, 50) = 50`) and 25 / 100 is rejected.
    /// Set an explicit lot-aligned `replenish_amount` when a larger hidden
    /// tranche is needed on such a book.
    ///
    /// Changing the lot size on a book that already holds resting orders
    /// does not re-validate existing levels — that is the caller's
    /// responsibility. Orders admitted under the old lot size keep resting
    /// as they are. Every quantity-carrying update re-validates the projected
    /// order, so such an order can be repaired through `UpdateQuantity`,
    /// `UpdatePriceAndQuantity` or `Replace` when only its visible tranche is
    /// misaligned (a 15 / 20 reserve on a new lot of 10 becomes 20 / 20);
    /// when the hidden tranche or the replenishment configuration is what
    /// fails, no update can correct it and the order must be cancelled and
    /// re-submitted with aligned tranches.
    ///
    /// # Arguments
    /// - `lot_size`: Minimum quantity increment. Must be > 0
    pub fn set_lot_size(&mut self, lot_size: u64) {
        self.lot_size = Some(lot_size);
    }

    /// Set or clear the minimum quantity increment from an [`Option`].
    ///
    /// `Some(l)` enables lot validation / rounding at `l`; `None` disables it.
    /// This is the setter the sequencer [`ReplayEngine`](crate::ReplayEngine)
    /// uses to inject `lot_size` into a fresh book before replay so that a
    /// journal produced by a lot-constrained book (e.g. `MarketOrderByAmount`
    /// rounding per level) reconstructs to the same structure.
    ///
    /// Intended for replay / restore on a **fresh** book. Changing the lot
    /// size on a book that already holds resting orders does not re-validate
    /// existing levels — that is the caller's responsibility.
    ///
    /// # Arguments
    /// - `lot_size`: `Some(increment)` to enable (increment must be > 0), or
    ///   `None` to disable validation.
    #[inline]
    pub fn set_lot_size_opt(&mut self, lot_size: Option<u64>) {
        self.lot_size = lot_size;
    }

    /// Returns the configured lot size, if any.
    ///
    /// `None` means lot size validation is disabled (all quantities accepted).
    #[must_use]
    #[inline]
    pub fn lot_size(&self) -> Option<u64> {
        self.lot_size
    }

    /// Set the minimum order size.
    ///
    /// Orders with `total_quantity() < min_order_size` are rejected with
    /// `OrderBookError::OrderSizeOutOfRange`.
    ///
    /// # Arguments
    /// - `size`: Minimum allowed order quantity
    pub fn set_min_order_size(&mut self, size: u64) {
        self.min_order_size = Some(size);
    }

    /// Set the maximum order size.
    ///
    /// Orders with `total_quantity() > max_order_size` are rejected with
    /// `OrderBookError::OrderSizeOutOfRange`.
    ///
    /// # Arguments
    /// - `size`: Maximum allowed order quantity
    pub fn set_max_order_size(&mut self, size: u64) {
        self.max_order_size = Some(size);
    }

    /// Returns the configured minimum order size, if any.
    ///
    /// `None` means no minimum size validation (default).
    #[must_use]
    #[inline]
    pub fn min_order_size(&self) -> Option<u64> {
        self.min_order_size
    }

    /// Returns the configured maximum order size, if any.
    ///
    /// `None` means no maximum size validation (default).
    #[must_use]
    #[inline]
    pub fn max_order_size(&self) -> Option<u64> {
        self.max_order_size
    }

    /// Set the Self-Trade Prevention mode.
    ///
    /// When set to a mode other than [`STPMode::None`], the matching engine
    /// checks `user_id` on incoming and resting orders to prevent self-trades.
    /// Orders with `Hash32::zero()` always bypass STP checks.
    ///
    /// # Arguments
    /// - `mode`: The STP mode to activate
    pub fn set_stp_mode(&mut self, mode: STPMode) {
        self.stp_mode = mode;
    }

    /// Returns the configured Self-Trade Prevention mode.
    ///
    /// [`STPMode::None`] means STP is disabled (default).
    #[must_use]
    #[inline]
    pub fn stp_mode(&self) -> STPMode {
        self.stp_mode
    }

    /// Set an order state tracker for explicit lifecycle tracking.
    ///
    /// When set, every order transition (Open, PartiallyFilled, Filled,
    /// Cancelled, Rejected) is recorded and queryable via
    /// [`order_status`](Self::order_status).
    pub fn set_order_state_tracker(&mut self, tracker: super::order_state::OrderStateTracker) {
        self.order_state_tracker = Some(tracker);
    }

    /// Returns the current status of an order, or `None` if no tracker
    /// is configured or the order is unknown.
    #[must_use]
    pub fn order_status(&self, order_id: Id) -> Option<super::order_state::OrderStatus> {
        self.order_state_tracker
            .as_ref()
            .and_then(|t| t.get(order_id))
    }

    /// Returns a reference to the order state tracker, if configured.
    #[must_use]
    pub fn order_state_tracker(&self) -> Option<&super::order_state::OrderStateTracker> {
        self.order_state_tracker.as_ref()
    }

    /// Returns the full transition history for an order.
    ///
    /// Each entry is a `(timestamp_ms, OrderStatus)` pair in chronological
    /// order. Timestamps come from the configured order state tracker's
    /// clock. For deterministic replay and consistent timestamps, configure
    /// the tracker with the same clock as this book.
    /// Returns `None` if no tracker is configured or the order ID was
    /// never submitted.
    #[must_use]
    pub fn get_order_history(
        &self,
        order_id: Id,
    ) -> Option<Vec<(u64, super::order_state::OrderStatus)>> {
        self.order_state_tracker
            .as_ref()
            .and_then(|t| t.get_history(order_id))
    }

    /// Returns the number of orders currently in an active state
    /// (`Open` or `PartiallyFilled`).
    ///
    /// Returns `0` if no tracker is configured.
    #[must_use]
    pub fn active_order_count(&self) -> usize {
        self.order_state_tracker
            .as_ref()
            .map(|t| t.active_count())
            .unwrap_or(0)
    }

    /// Returns the number of orders currently in a terminal state
    /// (`Filled`, `Cancelled`, or `Rejected`).
    ///
    /// Returns `0` if no tracker is configured.
    #[must_use]
    pub fn terminal_order_count(&self) -> usize {
        self.order_state_tracker
            .as_ref()
            .map(|t| t.terminal_count())
            .unwrap_or(0)
    }

    /// Remove all terminal-state entries whose last transition is older
    /// than `older_than` ago.
    ///
    /// Active orders are never purged. Returns the number of entries
    /// removed, or `0` if no tracker is configured.
    pub fn purge_terminal_states(&self, older_than: std::time::Duration) -> usize {
        self.order_state_tracker
            .as_ref()
            .map(|t| t.purge_terminal_older_than(older_than))
            .unwrap_or(0)
    }

    /// Create a new order book for the given symbol with Self-Trade Prevention.
    ///
    /// # Arguments
    /// - `symbol`: The trading symbol for this order book
    /// - `stp_mode`: The STP mode to activate
    ///
    /// # Returns
    /// A new `OrderBook` instance with STP enabled
    pub fn with_stp_mode(symbol: &str, stp_mode: STPMode) -> Self {
        let mut book = Self::new(symbol);
        book.stp_mode = stp_mode;
        book
    }

    /// Get the symbol of this order book
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Set the market close timestamp for DAY orders.
    ///
    /// `timestamp` is **milliseconds since the Unix epoch** — the same clock
    /// unit `has_expired` compares against (`self.clock().now_millis()`). A
    /// `Day` order is expired once the current time reaches this value; passing
    /// a seconds-based timestamp would place market close in 1970 and expire
    /// every `Day` order immediately.
    pub fn set_market_close_timestamp(&self, timestamp: u64) {
        self.market_close_timestamp
            .store(timestamp, Ordering::SeqCst);
        self.has_market_close.store(true, Ordering::SeqCst);
        trace!(
            "Order book {}: Set market close timestamp to {}",
            self.symbol, timestamp
        );
    }

    /// Clear the market close timestamp
    pub fn clear_market_close_timestamp(&self) {
        self.has_market_close.store(false, Ordering::SeqCst);
    }

    /// Get the best bid price, if any
    ///
    /// # Performance
    /// O(1) operation using SkipMap's ordered structure (highest price is last)
    pub fn best_bid(&self) -> Option<u128> {
        if let Some(cached_bid) = self.cache.get_cached_best_bid() {
            return Some(cached_bid);
        }

        // SkipMap maintains sorted order, best bid (highest price) is last
        let best_price = self.bids.iter().next_back().map(|entry| *entry.key());

        // Update only the bid slot — never evict the ask side.
        self.cache.update_best_bid(best_price);

        best_price
    }

    /// Get the best ask price, if any
    ///
    /// # Performance
    /// O(1) operation using SkipMap's ordered structure (lowest price is first)
    pub fn best_ask(&self) -> Option<u128> {
        if let Some(cached_ask) = self.cache.get_cached_best_ask() {
            return Some(cached_ask);
        }

        // SkipMap maintains sorted order, best ask (lowest price) is first
        let best_price = self.asks.iter().next().map(|entry| *entry.key());

        // Update only the ask slot — never evict the bid side.
        self.cache.update_best_ask(best_price);

        best_price
    }

    /// Integer midpoint of the best bid and best ask, in price units,
    /// rounded down; `None` when either side is empty.
    ///
    /// Exact for every `u128` pair: `u128::midpoint` computes
    /// `(bid + ask) / 2` without the intermediate sum overflowing and
    /// without the `f64` round trip of [`Self::mid_price`] (which loses
    /// precision above 2^53 and whose `as u128` cast truncates). Used for
    /// pegged repricing and the `Mid` risk reference price (#245).
    #[inline]
    #[must_use]
    pub(crate) fn integer_mid_price(&self) -> Option<u128> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some(bid.midpoint(ask)),
            _ => None,
        }
    }

    /// Get the mid price (average of best bid and best ask)
    pub fn mid_price(&self) -> Option<f64> {
        match (
            OrderBook::<T>::best_bid(self),
            OrderBook::<T>::best_ask(self),
        ) {
            (Some(bid), Some(ask)) => Some((bid as f64 + ask as f64) / 2.0),
            _ => None,
        }
    }

    /// Get the last trade price, if any
    pub fn last_trade_price(&self) -> Option<u128> {
        if self.has_traded.load(Ordering::Relaxed) {
            Some(self.last_trade_price.load())
        } else {
            None
        }
    }

    /// Get the spread (best ask - best bid), in price ticks.
    ///
    /// Returns `None` when either side is empty, and also when the two
    /// best prices, read one after the other, describe a crossed book
    /// (best ask below best bid). A resting book is never crossed, so the
    /// latter only happens when a concurrent submit moves the top of book
    /// between the two reads; the difference is then not a spread and is
    /// not reported as one (it used to be clamped to `0`, #250). A locked
    /// read (equal prices) returns `Some(0)`.
    #[must_use]
    pub fn spread(&self) -> Option<u128> {
        match (
            OrderBook::<T>::best_bid(self),
            OrderBook::<T>::best_ask(self),
        ) {
            (Some(bid), Some(ask)) => ask.checked_sub(bid),
            _ => None,
        }
    }

    /// Finds the price where cumulative depth reaches the target quantity
    ///
    /// # Arguments
    /// - `target_depth`: The target cumulative quantity to reach
    /// - `side`: The side of the order book (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// `Ok(Some(price))` at which the cumulative depth reaches or exceeds the
    /// target, or `Ok(None)` if the target depth cannot be reached with
    /// available liquidity.
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a walked level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when the cumulative depth
    ///   overflows `u64` before the target is reached.
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels needed to reach the target depth.
    /// Leverages SkipMap's natural ordering for efficient iteration.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::Side;
    ///
    /// let orderbook = OrderBook::<()>::new("BTC/USD");
    /// // Find where 50 units of cumulative depth is reached
    /// if let Some(price) = orderbook.price_at_depth(50, Side::Buy)? {
    ///     println!("50 units cumulative depth reached at price: {}", price);
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn price_at_depth(
        &self,
        target_depth: u64,
        side: Side,
    ) -> Result<Option<u128>, OrderBookError> {
        Ok(self
            .cumulative_depth_to_target(target_depth, side)?
            .map(|(price, _)| price))
    }

    /// Shared walk behind [`Self::price_at_depth`] and
    /// [`Self::cumulative_depth_to_target`].
    fn depth_to_target_walk(
        &self,
        target_depth: u64,
        side: Side,
    ) -> Result<Option<(u128, u64)>, OrderBookError> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return Ok(None);
        }

        let mut cumulative = 0u64;

        // Iterate in price-priority order
        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter().rev()), // Highest to lowest
            Side::Sell => Either::Right(price_levels.iter()),     // Lowest to highest
        };

        for entry in iter {
            let price = *entry.key();
            let quantity = level_total(entry.value())?;
            cumulative = checked_depth_add(cumulative, quantity, "cumulative depth")?;

            if cumulative >= target_depth {
                return Ok(Some((price, cumulative)));
            }
        }

        Ok(None)
    }

    /// Returns both the price and actual cumulative depth when target is reached
    ///
    /// # Arguments
    /// - `target_depth`: The target cumulative quantity to reach
    /// - `side`: The side of the order book (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// `Ok(Some((price, cumulative_depth)))` where the cumulative depth
    /// reaches or exceeds the target, or `Ok(None)` if the target depth
    /// cannot be reached.
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a walked level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when the cumulative depth
    ///   overflows `u64` before the target is reached.
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels needed to reach the target depth.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::Side;
    ///
    /// let orderbook = OrderBook::<()>::new("BTC/USD");
    /// // Get both price and actual depth
    /// if let Some((price, depth)) = orderbook.cumulative_depth_to_target(50, Side::Buy)? {
    ///     println!("Target depth 50 reached at {} (actual: {})", price, depth);
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn cumulative_depth_to_target(
        &self,
        target_depth: u64,
        side: Side,
    ) -> Result<Option<(u128, u64)>, OrderBookError> {
        self.depth_to_target_walk(target_depth, side)
    }

    /// Calculates total depth available in the first N price levels
    ///
    /// # Arguments
    /// - `levels`: The number of price levels to include (from best price)
    /// - `side`: The side of the order book (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// The total cumulative quantity across the specified number of levels.
    /// Returns `Ok(0)` if the side is empty or if levels is 0.
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a summed level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when the sum overflows `u64`.
    ///
    /// # Performance
    /// O(min(levels, N) * log N) where N is the total number of price levels.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::Side;
    ///
    /// let orderbook = OrderBook::<()>::new("BTC/USD");
    /// // Total depth in top 10 bid levels
    /// let top_10_depth = orderbook.total_depth_at_levels(10, Side::Buy)?;
    /// println!("Total depth in top 10 bid levels: {}", top_10_depth);
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn total_depth_at_levels(&self, levels: usize, side: Side) -> Result<u64, OrderBookError> {
        if levels == 0 {
            return Ok(0);
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return Ok(0);
        }

        let mut total = 0u64;

        // Iterate in price-priority order
        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter().rev()), // Highest to lowest
            Side::Sell => Either::Right(price_levels.iter()),     // Lowest to highest
        };

        for entry in iter.take(levels) {
            total = checked_depth_add(total, level_total(entry.value())?, "total depth")?;
        }

        Ok(total)
    }

    /// Returns the absolute spread (ask - bid) in price units
    ///
    /// This is an alias for `spread()` provided for API consistency.
    ///
    /// # Returns
    /// - `Some(spread)` if both best bid and best ask exist
    /// - `None` if either side is empty
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 105, 10, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// if let Some(spread) = book.spread_absolute() {
    ///     println!("Absolute spread: {}", spread); // 5
    /// }
    /// ```
    #[must_use]
    pub fn spread_absolute(&self) -> Option<u128> {
        self.spread()
    }

    /// Returns the spread in basis points (bps)
    ///
    /// Basis points are calculated as: ((ask - bid) / mid_price) * multiplier
    /// One basis point = 0.01% = 0.0001
    ///
    /// # Arguments
    /// - `bps_multiplier`: Optional custom multiplier for basis points calculation.
    ///   If `None`, uses the default value of 10,000.
    ///   Common values: 10,000 for bps, 1,000,000 for pips in FX
    ///
    /// # Returns
    /// - `Some(bps)` if both best bid and best ask exist
    /// - `None` if either side is empty, mid price is zero, or the two best
    ///   prices read as crossed (see [`Self::spread`], #250)
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 10000, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 10010, 10, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// // Using default 10,000 multiplier
    /// if let Some(spread_bps) = book.spread_bps(None) {
    ///     println!("Spread: {:.2} bps", spread_bps); // ~10 bps
    /// }
    ///
    /// // Using custom multiplier for percentage
    /// if let Some(spread_pct) = book.spread_bps(Some(100.0)) {
    ///     println!("Spread: {:.2}%", spread_pct); // ~0.10%
    /// }
    /// ```
    #[must_use]
    pub fn spread_bps(&self, bps_multiplier: Option<f64>) -> Option<f64> {
        let multiplier = bps_multiplier.unwrap_or(DEFAULT_BASIS_POINTS_MULTIPLIER);

        match (self.best_bid(), self.best_ask(), self.mid_price()) {
            (Some(bid), Some(ask), Some(mid)) if mid > 0.0 => {
                let spread = ask.checked_sub(bid)? as f64;
                Some((spread / mid) * multiplier)
            }
            _ => None,
        }
    }

    /// Calculates the volume-weighted average price (VWAP) for a given quantity
    ///
    /// VWAP walks through price levels in order until the target quantity is filled,
    /// calculating the weighted average price based on the quantities at each level.
    ///
    /// # Arguments
    /// - `quantity`: The target quantity to fill (in units)
    /// - `side`: The side to calculate VWAP for (Buy = execute against asks, Sell = execute against bids)
    ///
    /// # Returns
    /// - `Ok(Some(vwap))` if sufficient liquidity exists to fill the quantity
    /// - `Ok(None)` if insufficient liquidity or quantity is zero
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a walked level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when the checked `u128`
    ///   notional (`price * quantity`, summed over the walked levels)
    ///   overflows.
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels needed to reach the target quantity.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Sell, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 105, 15, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// // Calculate VWAP for buying 20 units
    /// if let Some(vwap) = book.vwap(20, Side::Buy)? {
    ///     println!("VWAP for buying 20 units: {:.2}", vwap);
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn vwap(&self, quantity: u64, side: Side) -> Result<Option<f64>, OrderBookError> {
        if quantity == 0 {
            return Ok(None);
        }

        // For Buy orders, we execute against asks (in ascending order)
        // For Sell orders, we execute against bids (in descending order)
        let price_levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        if price_levels.is_empty() {
            return Ok(None);
        }

        let mut remaining = quantity;
        let mut total_cost = 0u128; // Checked u128 notional (#245)
        let mut total_filled = 0u64;

        // Iterate in price-priority order
        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter()), // Lowest to highest (asks)
            Side::Sell => Either::Right(price_levels.iter().rev()), // Highest to lowest (bids)
        };

        for entry in iter {
            if remaining == 0 {
                break;
            }

            let price = *entry.key();
            let available = level_total(entry.value())?;

            if available == 0 {
                continue;
            }

            // `fill_qty <= remaining <= quantity`, and `total_filled +
            // remaining == quantity` is kept invariant, so the quantity
            // updates below cannot fail; they stay checked regardless.
            let fill_qty = remaining.min(available);
            total_cost = checked_notional_add(total_cost, price, fill_qty, "vwap notional")?;
            total_filled = checked_depth_add(total_filled, fill_qty, "vwap filled quantity")?;
            remaining = remaining
                .checked_sub(fill_qty)
                .ok_or_else(|| analytics_overflow("vwap remaining quantity"))?;
        }

        if total_filled == quantity {
            Ok(Some(total_cost as f64 / total_filled as f64))
        } else {
            Ok(None) // Insufficient liquidity
        }
    }

    /// Calculates the micro price (weighted price by volume at best bid and ask)
    ///
    /// The micro price is calculated as:
    /// `(best_ask * bid_volume + best_bid * ask_volume) / (bid_volume + ask_volume)`
    ///
    /// This metric gives more weight to the side with more volume, providing
    /// a better estimate of the "true" price than the simple mid price.
    ///
    /// # Returns
    /// - `Ok(Some(micro_price))` if both best bid and best ask exist with non-zero volumes
    /// - `Ok(None)` if either side is empty or both volumes are zero
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a best level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when the two best-level
    ///   volumes overflow `u64` together.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 50, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 105, 30, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// if let Some(micro) = book.micro_price()? {
    ///     println!("Micro price: {:.2}", micro);
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn micro_price(&self) -> Result<Option<f64>, OrderBookError> {
        let (Some(best_bid_price), Some(best_ask_price)) = (self.best_bid(), self.best_ask())
        else {
            return Ok(None);
        };

        // Get volumes at best levels (a level removed concurrently between
        // the cache read and this lookup reads as "no quote").
        let (Some(bid_entry), Some(ask_entry)) = (
            self.bids.get(&best_bid_price),
            self.asks.get(&best_ask_price),
        ) else {
            return Ok(None);
        };
        let bid_volume = level_total(bid_entry.value())?;
        let ask_volume = level_total(ask_entry.value())?;

        let total_volume = checked_depth_add(bid_volume, ask_volume, "micro price volume")?;

        if total_volume == 0 {
            return Ok(None);
        }

        // micro_price = (ask_price * bid_volume + bid_price * ask_volume) / (bid_volume + ask_volume)
        let numerator = (best_ask_price as f64 * bid_volume as f64)
            + (best_bid_price as f64 * ask_volume as f64);
        let denominator = total_volume as f64;

        Ok(Some(numerator / denominator))
    }

    /// Calculates the order book imbalance ratio for the top N levels
    ///
    /// The imbalance is calculated as:
    /// `(bid_volume - ask_volume) / (bid_volume + ask_volume)`
    ///
    /// # Arguments
    /// - `levels`: Number of top price levels to consider (must be > 0)
    ///
    /// # Returns
    /// - A value between -1.0 and 1.0:
    ///   - `> 0`: More buy pressure (bids dominate)
    ///   - `< 0`: More sell pressure (asks dominate)
    ///   - `≈ 0`: Balanced order book
    ///   - Returns `Ok(0.0)` if both sides are empty or `levels` is 0
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a summed level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when either side's depth or
    ///   their sum overflows `u64`.
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels requested.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 60, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 105, 40, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// let imbalance = book.order_book_imbalance(5)?;
    /// if imbalance > 0.0 {
    ///     println!("More buy pressure: {:.2}", imbalance);
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn order_book_imbalance(&self, levels: usize) -> Result<f64, OrderBookError> {
        if levels == 0 {
            return Ok(0.0);
        }

        let bid_volume = self.total_depth_at_levels(levels, Side::Buy)?;
        let ask_volume = self.total_depth_at_levels(levels, Side::Sell)?;

        let total_volume = checked_depth_add(bid_volume, ask_volume, "imbalance volume")?;

        if total_volume == 0 {
            return Ok(0.0);
        }

        let bid_f64 = bid_volume as f64;
        let ask_f64 = ask_volume as f64;

        Ok((bid_f64 - ask_f64) / (bid_f64 + ask_f64))
    }

    /// Calculates the market impact of a hypothetical order
    ///
    /// Analyzes how an order would affect the market by walking through
    /// available liquidity and calculating key metrics including average price,
    /// slippage, and the number of levels consumed.
    ///
    /// # Arguments
    /// - `quantity`: The order quantity to analyze (in units)
    /// - `side`: The side of the order (Buy = execute against asks, Sell = execute against bids)
    ///
    /// # Returns
    /// A `MarketImpact` struct containing:
    /// - `avg_price`: Volume-weighted average execution price
    /// - `worst_price`: Furthest price from the best price
    /// - `slippage`: Absolute difference from best price
    /// - `slippage_bps`: Slippage in basis points
    /// - `levels_consumed`: Number of price levels the order would consume
    /// - `total_quantity_available`: Total resting depth on the hit side
    ///   (summed across every level, not capped at `quantity`)
    ///
    /// Note the two reference frames in the returned struct: `avg_price`,
    /// `worst_price`, `slippage`, `slippage_bps`, and `levels_consumed`
    /// describe only the portion this `quantity` would consume, while
    /// `total_quantity_available` reports the whole side's resting depth.
    /// A `quantity == 0` query is a degenerate no-op and returns an
    /// all-zero [`MarketImpact`] (including `total_quantity_available == 0`),
    /// so read depth with a positive `quantity`.
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a scanned level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when the checked `u128`
    ///   notional of the consumed portion, or the side's total resting
    ///   depth (`u64`), overflows.
    ///
    /// # Performance
    /// O(N) over the resting price levels on the side being hit: the impact
    /// metrics only need the consumed prefix, but `total_quantity_available`
    /// reports true depth, so the whole side is scanned. This is an
    /// analytics query, not on the matching hot path.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Sell, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 105, 15, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// let impact = book.market_impact(20, Side::Buy)?;
    /// println!("Average price: {}", impact.avg_price);
    /// println!("Slippage: {} bps", impact.slippage_bps);
    /// println!("Levels consumed: {}", impact.levels_consumed);
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn market_impact(&self, quantity: u64, side: Side) -> Result<MarketImpact, OrderBookError> {
        if quantity == 0 {
            return Ok(MarketImpact::empty());
        }

        // For Buy orders, we execute against asks (in ascending order)
        // For Sell orders, we execute against bids (in descending order)
        let price_levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        if price_levels.is_empty() {
            return Ok(MarketImpact::empty());
        }

        let best_price = match side {
            Side::Buy => self.best_ask(),
            Side::Sell => self.best_bid(),
        };

        let best_price = match best_price {
            Some(price) => price,
            None => return Ok(MarketImpact::empty()),
        };

        let mut remaining = quantity;
        let mut total_cost = 0u128;
        let mut total_filled = 0u64;
        let mut total_available = 0u64;
        let mut worst_price = best_price;
        let mut levels_consumed = 0usize;

        // Iterate in price-priority order. The loop scans the whole side
        // (not just the levels this order would consume) so
        // `total_quantity_available` reflects the *true* resting depth — the
        // `can_fill` / `fill_ratio` helpers are meaningless when it is capped
        // at the requested quantity. The impact metrics (`avg_price`,
        // `worst_price`, `slippage`, `levels_consumed`) still describe only
        // the consumed portion, gated on `remaining > 0`.
        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter()), // Lowest to highest (asks)
            Side::Sell => Either::Right(price_levels.iter().rev()), // Highest to lowest (bids)
        };

        for entry in iter {
            let price = *entry.key();
            let available = level_total(entry.value())?;

            if available == 0 {
                continue;
            }

            // Accumulate full available depth across every non-empty level.
            total_available =
                checked_depth_add(total_available, available, "market impact available depth")?;

            // Cost / slippage only reflect the portion this order consumes.
            if remaining > 0 {
                levels_consumed = levels_consumed
                    .checked_add(1)
                    .ok_or_else(|| analytics_overflow("market impact levels consumed"))?;
                let fill_qty = remaining.min(available);
                total_cost =
                    checked_notional_add(total_cost, price, fill_qty, "market impact notional")?;
                total_filled =
                    checked_depth_add(total_filled, fill_qty, "market impact filled quantity")?;
                worst_price = price;
                remaining = remaining
                    .checked_sub(fill_qty)
                    .ok_or_else(|| analytics_overflow("market impact remaining quantity"))?;
            }
        }

        let avg_price = if total_filled > 0 {
            total_cost as f64 / total_filled as f64
        } else {
            0.0
        };

        // Levels are walked away from the best price, so on a settled book
        // `worst_price` is never better than `best_price` and this is the
        // signed-free distance `worst - best` (Buy) / `best - worst` (Sell).
        // `abs_diff` is exact and cannot overflow; under a concurrent
        // best-price move between the cache read and the walk it reports
        // the true distance instead of collapsing to zero.
        let slippage = worst_price.abs_diff(best_price);

        let slippage_bps = if best_price > 0 {
            (slippage as f64 / best_price as f64) * DEFAULT_BASIS_POINTS_MULTIPLIER
        } else {
            0.0
        };

        Ok(MarketImpact {
            avg_price,
            worst_price,
            slippage,
            slippage_bps,
            levels_consumed,
            total_quantity_available: total_available,
        })
    }

    /// Simulates the execution of a market order
    ///
    /// Provides a detailed step-by-step simulation of how a market order
    /// would be filled, including all individual fills at different price levels.
    ///
    /// # Arguments
    /// - `quantity`: The order quantity to simulate (in units)
    /// - `side`: The side of the order (Buy = execute against asks, Sell = execute against bids)
    ///
    /// # Returns
    /// An `OrderSimulation` struct containing:
    /// - `fills`: Vector of (price, quantity) pairs for each fill
    /// - `avg_price`: Volume-weighted average execution price
    /// - `total_filled`: Total quantity that would be filled
    /// - `remaining_quantity`: Quantity that could not be filled
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a walked level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when the checked `u128`
    ///   notional of the simulated fills overflows.
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels needed.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Sell, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 105, 15, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// let simulation = book.simulate_market_order(20, Side::Buy)?;
    /// for (price, qty) in &simulation.fills {
    ///     println!("Fill: {} @ {}", qty, price);
    /// }
    /// println!("Average price: {}", simulation.avg_price);
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn simulate_market_order(
        &self,
        quantity: u64,
        side: Side,
    ) -> Result<OrderSimulation, OrderBookError> {
        if quantity == 0 {
            return Ok(OrderSimulation::empty());
        }

        // For Buy orders, we execute against asks (in ascending order)
        // For Sell orders, we execute against bids (in descending order)
        let price_levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        if price_levels.is_empty() {
            let mut sim = OrderSimulation::empty();
            sim.remaining_quantity = quantity;
            return Ok(sim);
        }

        let mut remaining = quantity;
        let mut total_cost = 0u128;
        let mut total_filled = 0u64;
        let mut fills = Vec::new();

        // Iterate in price-priority order
        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter()), // Lowest to highest (asks)
            Side::Sell => Either::Right(price_levels.iter().rev()), // Highest to lowest (bids)
        };

        for entry in iter {
            if remaining == 0 {
                break;
            }

            let price = *entry.key();
            let available = level_total(entry.value())?;

            if available == 0 {
                continue;
            }

            let fill_qty = remaining.min(available);
            total_cost = checked_notional_add(total_cost, price, fill_qty, "simulation notional")?;
            total_filled = checked_depth_add(total_filled, fill_qty, "simulation filled quantity")?;
            fills.push((price, fill_qty));
            remaining = remaining
                .checked_sub(fill_qty)
                .ok_or_else(|| analytics_overflow("simulation remaining quantity"))?;
        }

        let avg_price = if total_filled > 0 {
            total_cost as f64 / total_filled as f64
        } else {
            0.0
        };

        Ok(OrderSimulation {
            fills,
            avg_price,
            total_filled,
            remaining_quantity: remaining,
        })
    }

    /// Calculates available liquidity within a specific price range
    ///
    /// Sums up the total quantity available at price levels that fall
    /// within the specified price range (inclusive).
    ///
    /// # Arguments
    /// - `min_price`: Minimum price of the range (inclusive, in price units)
    /// - `max_price`: Maximum price of the range (inclusive, in price units)
    /// - `side`: The side to analyze (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// Total quantity available in the specified price range (in units);
    /// `Ok(0)` for an empty side or an inverted range.
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when an in-range level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when the sum overflows `u64`.
    ///
    /// # Performance
    /// O(log N + M) where M is the number of levels in the range.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 105, 15, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 110, 20, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Get liquidity between 100 and 105 (inclusive)
    /// let liquidity = book.liquidity_in_range(100, 105, Side::Buy)?;
    /// assert_eq!(liquidity, 25); // 10 + 15
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn liquidity_in_range(
        &self,
        min_price: u128,
        max_price: u128,
        side: Side,
    ) -> Result<u64, OrderBookError> {
        if min_price > max_price {
            return Ok(0);
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        let mut total_liquidity = 0u64;

        for entry in price_levels.range(min_price..=max_price) {
            total_liquidity = checked_depth_add(
                total_liquidity,
                level_total(entry.value())?,
                "liquidity in range",
            )?;
        }

        Ok(total_liquidity)
    }

    /// Returns the number of orders ahead in queue at a specific price level
    ///
    /// Calculates how many orders are already in the queue at the specified
    /// price level. Useful for estimating execution probability and queue position.
    ///
    /// # Arguments
    /// - `price`: The price level to check (in price units)
    /// - `side`: The side to check (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// The number of orders at that price level. Returns 0 if the price level doesn't exist.
    ///
    /// # Performance
    /// O(1) for price level lookup, O(N) for counting orders where N is orders at that level.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 20, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// let orders_ahead = book.queue_ahead_at_price(100, Side::Buy);
    /// assert_eq!(orders_ahead, 2);
    /// ```
    #[must_use]
    pub fn queue_ahead_at_price(&self, price: u128, side: Side) -> usize {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if let Some(entry) = price_levels.get(&price) {
            entry.value().iter_orders().count()
        } else {
            0
        }
    }

    /// Calculates the price N ticks inside the best price
    ///
    /// Useful for placing orders that are competitive but not at the best price.
    /// For buy orders, "inside" means lower than best bid.
    /// For sell orders, "inside" means higher than best ask.
    ///
    /// # Arguments
    /// - `n_ticks`: Number of ticks to move inside (in ticks)
    /// - `tick_size`: The size of each tick (in price units)
    /// - `side`: The side to calculate for (Buy or Sell)
    ///
    /// # Returns
    /// - `Some(price)` if best price exists and calculation is valid
    /// - `None` if no best price exists or calculation would underflow/overflow
    ///
    /// # Performance
    /// O(1) operation using cached best prices.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 105, 10, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// // Buy side: best bid is 100, 1 tick inside = 99 (if tick_size = 1)
    /// if let Some(price) = book.price_n_ticks_inside(1, 1, Side::Buy) {
    ///     assert_eq!(price, 99);
    /// }
    ///
    /// // Sell side: best ask is 105, 1 tick inside = 106 (if tick_size = 1)
    /// if let Some(price) = book.price_n_ticks_inside(1, 1, Side::Sell) {
    ///     assert_eq!(price, 106);
    /// }
    /// ```
    #[must_use]
    pub fn price_n_ticks_inside(
        &self,
        n_ticks: usize,
        tick_size: u128,
        side: Side,
    ) -> Option<u128> {
        if n_ticks == 0 || tick_size == 0 {
            return None;
        }

        let adjustment = (n_ticks as u128).checked_mul(tick_size)?;

        match side {
            Side::Buy => {
                let best_bid = self.best_bid()?;
                best_bid.checked_sub(adjustment)
            }
            Side::Sell => {
                let best_ask = self.best_ask()?;
                best_ask.checked_add(adjustment)
            }
        }
    }

    /// Calculates the optimal price to be at a specific queue position
    ///
    /// Determines what price level would place you at the Nth position in the queue.
    /// Position 1 means front of queue (best price), position 2 means second-best, etc.
    ///
    /// # Arguments
    /// - `position`: Target queue position (1 = best price, 2 = second best, etc.)
    /// - `side`: The side to calculate for (Buy or Sell)
    ///
    /// # Returns
    /// - `Some(price)` if the position exists in the order book
    /// - `None` if position is 0 or exceeds available price levels
    ///
    /// # Performance
    /// O(N) where N is the target position, due to iteration through price levels.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 99, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 98, 10, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Position 1 should be best bid (100)
    /// assert_eq!(book.price_for_queue_position(1, Side::Buy), Some(100));
    /// // Position 2 should be second best (99)
    /// assert_eq!(book.price_for_queue_position(2, Side::Buy), Some(99));
    /// ```
    #[must_use]
    pub fn price_for_queue_position(&self, position: usize, side: Side) -> Option<u128> {
        if position == 0 {
            return None;
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return None;
        }

        // For bids: iterate from highest to lowest (reverse)
        // For asks: iterate from lowest to highest (forward)
        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter().rev()),
            Side::Sell => Either::Right(price_levels.iter()),
        };

        for (current_position, entry) in (1usize..).zip(iter) {
            if current_position == position {
                return Some(*entry.key());
            }
        }

        None
    }

    /// Suggests optimal price to place an order just inside a target depth
    ///
    /// Calculates the price level where placing an order would position it
    /// just inside (better than) the specified cumulative depth. Useful for
    /// depth-based market making strategies.
    ///
    /// # Arguments
    /// - `target_depth`: Target cumulative quantity (in units)
    /// - `tick_size`: The size of each tick (in price units)
    /// - `side`: The side to calculate for (Buy or Sell)
    ///
    /// # Returns
    /// - `Ok(Some(price))` adjusted by one tick inside the depth level
    /// - `Ok(Some(deepest_price))` when the side cannot reach `target_depth`
    /// - `Ok(None)` for a zero target / tick, an empty side, or a one-tick
    ///   adjustment that leaves the `u128` price domain
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a walked level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when the cumulative depth
    ///   overflows `u64` before the target is reached.
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels to reach target depth.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 50, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 99, 60, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 98, 70, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Want to be just inside 100 units of depth
    /// // Depth at 100: 50, at 99: 110, so we want to be at 100 (just inside 110)
    /// if let Some(price) = book.price_at_depth_adjusted(100, 1, Side::Buy)? {
    ///     assert_eq!(price, 100); // One tick better than the level that reaches depth
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn price_at_depth_adjusted(
        &self,
        target_depth: u64,
        tick_size: u128,
        side: Side,
    ) -> Result<Option<u128>, OrderBookError> {
        if target_depth == 0 || tick_size == 0 {
            return Ok(None);
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return Ok(None);
        }

        let mut cumulative_depth = 0u64;
        let mut last_price = None;

        // For bids: iterate from highest to lowest (reverse)
        // For asks: iterate from lowest to highest (forward)
        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter().rev()),
            Side::Sell => Either::Right(price_levels.iter()),
        };

        for entry in iter {
            let price = *entry.key();
            let quantity = level_total(entry.value())?;
            cumulative_depth = checked_depth_add(cumulative_depth, quantity, "cumulative depth")?;

            if cumulative_depth >= target_depth {
                // Found the level where we exceed target depth
                // Return one tick better than this price
                return Ok(match side {
                    Side::Buy => price.checked_add(tick_size),
                    Side::Sell => price.checked_sub(tick_size),
                });
            }

            last_price = Some(price);
        }

        // If we didn't reach target depth, return the last price seen
        // (deepest level available)
        Ok(last_price)
    }

    /// Returns an iterator over price levels with cumulative depth tracking
    ///
    /// Iterates through price levels in price-priority order (best to worst),
    /// maintaining cumulative depth as it progresses. This provides a memory-efficient
    /// way to analyze market depth distribution without allocating vectors.
    ///
    /// # Arguments
    /// - `side`: The side to iterate (Buy for bids from highest to lowest, Sell for asks from lowest to highest)
    ///
    /// # Returns
    /// An iterator yielding `Result<LevelInfo, OrderBookError>` containing
    /// price, quantity, and cumulative depth. A level whose
    /// `visible + hidden` total overflows `u64`
    /// ([`OrderBookError::PriceLevelError`]) or a cumulative depth that
    /// overflows `u64` ([`OrderBookError::ArithmeticOverflow`]) is yielded
    /// once as `Err`, after which the iterator is exhausted.
    ///
    /// # Performance
    /// Lazy evaluation with O(1) memory overhead. Each iteration is O(log N) for skipmap traversal.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 99, 15, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 98, 20, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Functional-style analysis
    /// for level in book.levels_with_cumulative_depth(Side::Buy).take(5) {
    ///     let level = level?;
    ///     println!("Price: {}, Qty: {}, Cumulative: {}",
    ///              level.price, level.quantity, level.cumulative_depth);
    ///     
    ///     if level.cumulative_depth >= 30 {
    ///         println!("Target depth reached!");
    ///         break;
    ///     }
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    #[must_use]
    pub fn levels_with_cumulative_depth(&self, side: Side) -> LevelsWithCumulativeDepth<'_> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        LevelsWithCumulativeDepth::new(price_levels, side)
    }

    /// Returns an iterator over price levels until target depth is reached
    ///
    /// Automatically stops when cumulative depth reaches or exceeds the target.
    /// This is useful for determining how many price levels are needed to fill
    /// a specific quantity, without processing unnecessary deeper levels.
    ///
    /// # Arguments
    /// - `target_depth`: Target cumulative quantity (in units)
    /// - `side`: The side to iterate (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// An iterator of `Result<LevelInfo, OrderBookError>` that stops when
    /// target depth is reached. A level-total or cumulative-depth overflow
    /// is yielded once as `Err` and ends the iteration (see
    /// [`Self::levels_with_cumulative_depth`]).
    ///
    /// # Performance
    /// Short-circuits early, processing only the minimum levels needed. O(M log N) where M is levels to reach target.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 99, 15, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 98, 20, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Collect levels needed for 30 units
    /// let levels = book
    ///     .levels_until_depth(30, Side::Buy)
    ///     .collect::<Result<Vec<_>, _>>()?;
    /// println!("Levels needed: {}", levels.len());
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    #[must_use]
    pub fn levels_until_depth(&self, target_depth: u64, side: Side) -> LevelsUntilDepth<'_> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        LevelsUntilDepth::new(price_levels, side, target_depth)
    }

    /// Returns an iterator over price levels within a specific price range
    ///
    /// Only yields levels where the price falls within [min_price, max_price] inclusive.
    /// Useful for analyzing liquidity distribution in specific price bands without
    /// allocating intermediate collections.
    ///
    /// # Arguments
    /// - `min_price`: Minimum price of the range (inclusive, in price units)
    /// - `max_price`: Maximum price of the range (inclusive, in price units)
    /// - `side`: The side to iterate (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// An iterator yielding `Result<LevelInfo, OrderBookError>` only for
    /// levels within the price range (`cumulative_depth` is not tracked and
    /// is always `0`). A level whose `visible + hidden` total overflows
    /// `u64` is yielded once as `Err` and ends the iteration.
    ///
    /// # Performance
    /// Skips levels outside range, O(M log N) where M is levels in range.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 95, 15, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 90, 20, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Analyze levels between 90 and 100
    /// for level in book.levels_in_range(90, 100, Side::Buy) {
    ///     let level = level?;
    ///     println!("{} units at {}", level.quantity, level.price);
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    #[must_use]
    pub fn levels_in_range(
        &self,
        min_price: u128,
        max_price: u128,
        side: Side,
    ) -> LevelsInRange<'_> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        LevelsInRange::new(price_levels, side, min_price, max_price)
    }

    /// Finds the first price level matching a predicate
    ///
    /// Searches through price levels in price-priority order and returns the first
    /// level that satisfies the given predicate function. The predicate receives
    /// both the level information and cumulative depth for context-aware decisions.
    ///
    /// # Arguments
    /// - `side`: The side to search (Buy for bids, Sell for asks)
    /// - `predicate`: Function that takes `LevelInfo` and returns `true` if the level matches
    ///
    /// # Returns
    /// - `Ok(Some(LevelInfo))` if a matching level is found
    /// - `Ok(None)` if no level matches or the book is empty
    ///
    /// # Errors
    /// Propagates the first level error of
    /// [`Self::levels_with_cumulative_depth`] reached before a match
    /// ([`OrderBookError::PriceLevelError`] or
    /// [`OrderBookError::ArithmeticOverflow`]).
    ///
    /// # Performance
    /// Short-circuits on first match, O(M log N) where M is position of match.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 5, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 99, 15, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 98, 25, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Find first level with quantity > 10
    /// if let Some(level) = book.find_level(Side::Buy, |info| info.quantity > 10)? {
    ///     println!("First large level at price: {}", level.price);
    /// }
    ///
    /// // Find first level where cumulative depth exceeds 20
    /// if let Some(level) = book.find_level(Side::Buy, |info| info.cumulative_depth > 20)? {
    ///     println!("Depth threshold at: {}", level.price);
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn find_level<F>(
        &self,
        side: Side,
        predicate: F,
    ) -> Result<Option<LevelInfo>, OrderBookError>
    where
        F: Fn(&LevelInfo) -> bool,
    {
        for level in self.levels_with_cumulative_depth(side) {
            let level = level?;
            if predicate(&level) {
                return Ok(Some(level));
            }
        }
        Ok(None)
    }

    /// Returns the visible (displayed) resting quantity at a price level, in
    /// quantity units, or `None` when no level exists there.
    ///
    /// O(log N) `SkipMap` point lookup followed by a single relaxed atomic
    /// load of the level's visible counter — no per-order [`Arc`] is
    /// materialized and no `T: Default` conversion is performed, so this is
    /// the cheap way to poll displayed depth at one price (contrast
    /// [`Self::get_orders_at_price`], which clones every order).
    ///
    /// # Arguments
    /// - `price`: The price level to read (in price units).
    /// - `side`: The side to read (`Buy` for bids, `Sell` for asks).
    ///
    /// # Returns
    /// - `Some(qty)` when a level exists at `price`. `Some(0)` denotes a live
    ///   level whose visible depth is momentarily zero (e.g. a fully-hidden
    ///   iceberg tranche) — distinct from `None`.
    /// - `None` when no level exists at `price` on that side.
    ///
    /// # Consistency
    /// This is an **advisory, eventually-consistent** counter read (see
    /// `pricelevel`'s `PriceLevel::visible_quantity`): under concurrent
    /// `add_order` / matching / `update_order` it can briefly lead or lag the
    /// queue contents, and it is not guaranteed mutually consistent with a
    /// separately-read [`Self::order_count_at_price`] /
    /// [`Self::hidden_quantity_at_price`] / [`Self::total_quantity_at_price`].
    /// For a view where the counters and the order list agree, take
    /// [`Self::create_snapshot`] and read from it.
    #[must_use]
    pub fn visible_quantity_at_price(&self, price: u128, side: Side) -> Option<u64> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        price_levels
            .get(&price)
            .map(|entry| entry.value().visible_quantity())
    }

    /// Returns the hidden (reserve) resting quantity at a price level, in
    /// quantity units, or `None` when no level exists there.
    ///
    /// The symmetric counterpart to [`Self::visible_quantity_at_price`]: same
    /// O(log N) lookup plus one relaxed atomic load of the level's hidden
    /// counter, no allocation, no `T: Default` bound. Hidden quantity is the
    /// undisplayed reserve of iceberg / reserve orders.
    ///
    /// # Arguments
    /// - `price`: The price level to read (in price units).
    /// - `side`: The side to read (`Buy` for bids, `Sell` for asks).
    ///
    /// # Returns
    /// - `Some(qty)` when a level exists at `price` (`Some(0)` = a live level
    ///   with no hidden reserve, distinct from `None`).
    /// - `None` when no level exists at `price` on that side.
    ///
    /// # Consistency
    /// **Advisory, eventually-consistent** counter read (see
    /// `pricelevel`'s `PriceLevel::hidden_quantity`): under concurrent
    /// mutation it can briefly lead or lag the queue, and it is not guaranteed
    /// mutually consistent with a separately-read visible / count / total. For
    /// a mutually-consistent view, take [`Self::create_snapshot`].
    #[must_use]
    pub fn hidden_quantity_at_price(&self, price: u128, side: Side) -> Option<u64> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        price_levels
            .get(&price)
            .map(|entry| entry.value().hidden_quantity())
    }

    /// Returns the total (visible + hidden) resting quantity at a price level,
    /// in quantity units, or `None` when no level exists there.
    ///
    /// O(log N) `SkipMap` point lookup plus two relaxed atomic loads summed by
    /// `pricelevel`'s `PriceLevel::total_quantity`. That sum returns a
    /// `Result` that errors on a `u64` overflow of `visible + hidden` (for
    /// example a limit order and an iceberg at one price whose combined
    /// depth exceeds `u64::MAX`); since 0.14.0 (#245) that overflow is
    /// surfaced as an error instead of being read as `u64::MAX`.
    ///
    /// # Arguments
    /// - `price`: The price level to read (in price units).
    /// - `side`: The side to read (`Buy` for bids, `Sell` for asks).
    ///
    /// # Returns
    /// - `Ok(Some(qty))` when a level exists at `price`. A `Some(0)` here is
    ///   only a brief concurrency transient during level removal — an empty
    ///   level is eagerly removed from the `SkipMap` on every removal path, so
    ///   a settled level always has positive total quantity.
    /// - `Ok(None)` when no level exists at `price` on that side.
    ///
    /// # Errors
    /// Returns [`OrderBookError::PriceLevelError`] when the level's
    /// `visible + hidden` total overflows `u64`.
    ///
    /// # Consistency
    /// **Advisory, eventually-consistent** read — it sums two independent
    /// atomic counters (see `pricelevel`'s `PriceLevel::visible_quantity`), so
    /// under concurrent mutation the two loads may straddle a mutation and it
    /// is not guaranteed mutually consistent with the per-side visible / hidden
    /// / count read separately. For a mutually-consistent view, take
    /// [`Self::create_snapshot`].
    pub fn total_quantity_at_price(
        &self,
        price: u128,
        side: Side,
    ) -> Result<Option<u64>, OrderBookError> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        price_levels
            .get(&price)
            .map(|entry| level_total(entry.value()))
            .transpose()
    }

    /// Returns the number of resting orders at a price level, or `None` when no
    /// level exists there.
    ///
    /// Cheaper counterpart to [`Self::queue_ahead_at_price`], which counts by
    /// iterating the level's order queue. Both do the same O(log N) `SkipMap`
    /// point lookup; this method then reads the level's maintained
    /// `order_count` atomic (a single relaxed load) instead of walking the K
    /// orders at the level, so it drops the per-order term: O(log N) here vs
    /// O(log N + K) for `queue_ahead_at_price`. Prefer this when you only need
    /// the count; [`Self::queue_ahead_at_price`] is left unchanged and still
    /// returns `0` (not `None`) for an absent level.
    ///
    /// # Arguments
    /// - `price`: The price level to read (in price units).
    /// - `side`: The side to read (`Buy` for bids, `Sell` for asks).
    ///
    /// # Returns
    /// - `Some(count)` when a level exists at `price`. A `Some(0)` here is only
    ///   a brief concurrency transient during level removal — an empty level is
    ///   eagerly removed from the `SkipMap` on every removal path, so a settled
    ///   level always has at least one order.
    /// - `None` when no level exists at `price` on that side.
    ///
    /// # Consistency
    /// **Advisory, eventually-consistent** counter read (see `pricelevel`'s
    /// `PriceLevel::order_count`): under concurrent mutation it can briefly
    /// lead or lag the queue and is not guaranteed mutually consistent with a
    /// separately-read quantity. For a mutually-consistent view, take
    /// [`Self::create_snapshot`].
    #[must_use]
    pub fn order_count_at_price(&self, price: u128, side: Side) -> Option<usize> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        price_levels
            .get(&price)
            .map(|entry| entry.value().order_count())
    }

    /// Get all orders at a specific price level
    pub fn get_orders_at_price(&self, price: u128, side: Side) -> Vec<Arc<OrderType<T>>>
    where
        T: Default,
    {
        trace!(
            "Order book {}: Getting orders at price {} for side {:?}",
            self.symbol, price, side
        );
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if let Some(entry) = price_levels.get(&price) {
            entry
                .value()
                .iter_orders()
                .map(|order| Arc::new(self.convert_from_unit_type(&order)))
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Get all orders in the book
    pub fn get_all_orders(&self) -> Vec<Arc<OrderType<T>>>
    where
        T: Default,
    {
        trace!("Order book {}: Getting all orders", self.symbol);
        let mut result = Vec::new();

        // Get all bid orders
        for item in self.bids.iter() {
            let price_level = item.value();
            let converted_orders: Vec<Arc<OrderType<T>>> = price_level
                .iter_orders()
                .map(|order| Arc::new(self.convert_from_unit_type(&order)))
                .collect();
            result.extend(converted_orders);
        }

        // Get all ask orders
        for item in self.asks.iter() {
            let price_level = item.value();
            let converted_orders: Vec<Arc<OrderType<T>>> = price_level
                .iter_orders()
                .map(|order| Arc::new(self.convert_from_unit_type(&order)))
                .collect();
            result.extend(converted_orders);
        }

        result
    }

    /// Evaluate the pending trailing stops (#286) before a mutating entry
    /// point that can trade returns, under the submit gate it holds. One
    /// relaxed load when no stop is pending; nothing without
    /// `special_orders`. See `stop_orders.rs`.
    #[inline]
    pub(super) fn settle_pending_stops(&self) {
        #[cfg(feature = "special_orders")]
        {
            let _pass = self.run_stop_triggers();
        }
    }

    /// Get an order by its ID.
    ///
    /// Finds resting orders and, under `special_orders`, pending trailing
    /// stops (#286), which rest on no level.
    pub fn get_order(&self, order_id: Id) -> Option<Arc<OrderType<T>>>
    where
        T: Default,
    {
        // Get the order location without locking
        if let Some(location) = self.order_locations.get(&order_id) {
            let (price, side) = location.price_side();

            let price_levels = match side {
                Side::Buy => &self.bids,
                Side::Sell => &self.asks,
            };

            // Get the price level
            if let Some(entry) = price_levels.get(&price) {
                let price_level = entry.value();
                // Iterate through the orders at this level to find the one with the matching ID
                for order in price_level.iter_orders() {
                    if order.id() == order_id {
                        return Some(Arc::new(self.convert_from_unit_type(&order)));
                    }
                }
            }
            return None;
        }

        #[cfg(feature = "special_orders")]
        if self.has_pending_stops() {
            return self.pending_stop_order(order_id);
        }
        None
    }

    /// Match a market order against the book.
    ///
    /// This is a convenience wrapper that bypasses STP (uses `Hash32::zero()`).
    /// Use [`Self::match_market_order_with_user`] when STP is needed.
    pub fn match_market_order(
        &self,
        order_id: Id,
        quantity: u64,
        side: Side,
    ) -> Result<MatchResult, OrderBookError> {
        self.match_market_order_with_user(order_id, quantity, side, Hash32::zero())
    }

    /// Match a market order against the book with Self-Trade Prevention.
    ///
    /// When STP is enabled and `user_id` is non-zero, the matching engine
    /// checks resting orders for same-user conflicts before executing fills.
    ///
    /// # Arguments
    /// * `order_id` — Unique identifier for this market order.
    /// * `quantity` — Quantity to match.
    /// * `side` — Buy or Sell.
    /// * `user_id` — Owner of the incoming order for STP checks.
    ///   Pass `Hash32::zero()` to bypass STP.
    ///
    /// # Errors
    /// Returns [`OrderBookError::InsufficientLiquidity`] when no liquidity
    /// is available, or [`OrderBookError::SelfTradePrevented`] when STP
    /// cancels the taker before any fills occur. Returns
    /// [`OrderBookError::MatchAborted`] when the sweep stopped at a price
    /// level that reported a failure (#240); the trades committed before it
    /// have already reached the trade listener, exactly like a partial fill.
    pub fn match_market_order_with_user(
        &self,
        order_id: Id,
        quantity: u64,
        side: Side,
        user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        self.match_market_order_committed(order_id, quantity, side, user_id, false)
            .map_err(SubmitFailure::into_error)
    }

    /// Shared body of the base-quantity market sweep: match, then publish
    /// the trades (including an aborted sweep's committed prefix, #240).
    /// `want_committed` builds the committed `TradeResult` for the caller
    /// even when no trade listener is installed.
    pub(crate) fn match_market_order_committed(
        &self,
        order_id: Id,
        quantity: u64,
        side: Side,
        user_id: Hash32,
        want_committed: bool,
    ) -> Result<MatchResult, SubmitFailure> {
        trace!(
            "Order book {}: Matching market order {} for {} at side {:?}",
            self.symbol, order_id, quantity, side
        );
        // #209 / #225: same gate as `match_order_with_user`. #249: the
        // trades are published under it too, so their `engine_seq` is
        // stamped at commit with the sweep's level events; the listeners
        // still run only after the gate is released.
        let _gate = self.acquire_coherent_submit_gate(
            self.submit_needs_exclusive_gate(false, user_id, false, false),
        );
        let result = (|| {
            // #240: under the same gate the sweep holds, before any mutation.
            self.check_trade_id_headroom(order_id, side, None)?;
            // #244: worst-case notional / fee representability, same place.
            let verified = self.check_trade_arithmetic_or_reject(order_id, side, quantity, None)?;
            let outcome = self.match_order_with_user_outcome(
                order_id,
                side,
                quantity,
                None,
                user_id,
                TakerKind::Standard,
                SweepReservation::NONE,
                verified,
            )?;
            self.publish_match_outcome(outcome, want_committed)
        })();
        // #286: under the same gate, after the taker's trades.
        self.settle_pending_stops();
        result
    }

    /// Reject a taker untouched when the trade-id generator is exhausted
    /// and the taker would trade (#240): its sweep could not mint a single
    /// trade id. A market taker (`limit_price == None`) trades whenever the
    /// opposite side holds liquidity; a limit taker only when its price
    /// crosses the best opposite price. Records `Rejected { CapacityExceeded }`
    /// and latches [`Self::trade_ids_exhausted`].
    ///
    /// Callers run it while holding the submit gate the sweep holds, before
    /// any mutation. It is exact under the exclusive gate (and for a single
    /// writer). Under the shared gate it is best-effort: pricelevel 0.10
    /// exposes no public atomic reservation on the generator, so concurrent
    /// takers racing for the last ids can all pass this check, and the ones
    /// that lose the race abort inside the sweep with
    /// [`OrderBookError::MatchAborted`] (their committed prefix published,
    /// possibly empty) instead of this untouched rejection.
    ///
    /// # Errors
    ///
    /// [`OrderBookError::PriceLevelError`] (`CapacityExceeded { IdSequence }`).
    #[inline]
    pub(crate) fn check_trade_id_headroom(
        &self,
        order_id: Id,
        side: Side,
        limit_price: Option<u128>,
    ) -> Result<(), OrderBookError> {
        if !self.transaction_id_generator.is_exhausted() {
            return Ok(());
        }
        let crosses = match limit_price {
            Some(price) => self.will_cross_market(price, side),
            None => match side {
                Side::Buy => !self.asks.is_empty(),
                Side::Sell => !self.bids.is_empty(),
            },
        };
        if !crosses {
            return Ok(());
        }
        let source = PriceLevelError::CapacityExceeded {
            resource: pricelevel::CapacityResource::IdSequence,
            additional: 1,
        };
        Err(self.reject_untouched(order_id, source))
    }

    /// Worst-case trade arithmetic preflight (#244).
    ///
    /// Before a base-quantity taker touches the book, bound the notional its
    /// sweep can reach — the worst price it can **reach** times `quantity` —
    /// and verify that the notional fits `u128` and that both legs of the
    /// configured [`FeeSchedule`] price it exactly. Every committed trade of
    /// the sweep then has a representable notional, `quote_notional` and
    /// fees (the per-trade fees and their sums are bounded by the fee on the
    /// bound), so the `TradeResult` is built without clamping.
    ///
    /// The worst reachable price is:
    ///
    /// - **Buy with a limit that passes**: the limit (no level read).
    /// - **Other buys**: asks are walked from the best one, accumulating
    ///   their visible quantity until `quantity` is covered (or the limit
    ///   is passed, or the side ends); the highest price visited is the
    ///   bound. A single far-away ask that the taker cannot reach never
    ///   rejects it, so a maker resting an absurd price cannot block
    ///   ordinary market buys. A market buy covered by the best level reads
    ///   one level.
    /// - **Sell**: the best bid (cache). A sell walks bids downward, so the
    ///   best bid is the highest price it can trade at, and fees grow with
    ///   price.
    ///
    /// Visible quantity is a lower bound on what a level fills (hidden
    /// iceberg / reserve depth at the same price only shortens the walk), so
    /// the walk never stops before a level the sweep reaches — except for
    /// makers the sweep skips without filling (self-trade prevention, a
    /// maker set aside for making no progress), for which the sweep's
    /// per-level backstop aborts instead.
    ///
    /// A taker that cannot trade (empty opposite side, a limit that does
    /// not cross) always passes. No allocation.
    ///
    /// Callers run it under the submit gate the sweep holds, before any
    /// mutation, next to [`Self::check_trade_id_headroom`]. Like that check
    /// it is exact under the exclusive gate (and for a single writer) and
    /// best-effort under the shared one: a maker admitted concurrently at a
    /// worse price is caught by the sweep's per-level backstop, which aborts
    /// with [`OrderBookError::MatchAborted`] before touching that level.
    ///
    /// # Returns
    ///
    /// The highest price verified: every price at or below it prices
    /// `quantity` exactly. The sweep seeds its backstop with it, so levels at
    /// or below cost one comparison. `0` when the taker cannot trade.
    ///
    /// # Errors
    ///
    /// [`OrderBookError::NotionalOverflow`] when `price × quantity`
    /// overflows `u128`; [`OrderBookError::FeeOverflow`] when a fee leg
    /// cannot price the bound exactly. Pure: nothing is recorded.
    #[inline]
    pub(crate) fn check_trade_arithmetic(
        &self,
        side: Side,
        quantity: u64,
        limit_price: Option<u128>,
    ) -> Result<u128, OrderBookError> {
        let schedule = self.active_fee_schedule();
        match side {
            Side::Buy => {
                let Some(best_ask) = self.best_ask() else {
                    return Ok(0);
                };
                if let Some(limit) = limit_price {
                    if limit < best_ask {
                        return Ok(0);
                    }
                    // A limit bounds every buy trade price.
                    if Self::check_notional_bound(schedule, limit, quantity).is_ok() {
                        return Ok(limit);
                    }
                }
                self.reachable_ask_bound(schedule, quantity, limit_price)
            }
            Side::Sell => {
                let Some(best_bid) = self.best_bid() else {
                    return Ok(0);
                };
                if limit_price.is_some_and(|limit| limit > best_bid) {
                    return Ok(0);
                }
                Self::check_notional_bound(schedule, best_bid, quantity)?;
                Ok(best_bid)
            }
        }
    }

    /// Walk the asks a buy of `quantity` can reach (capped at `limit_price`)
    /// and verify each new highest price; see
    /// [`Self::check_trade_arithmetic`]. Returns the highest price verified.
    ///
    /// # Errors
    ///
    /// The first reachable price whose bound fails.
    #[inline]
    fn reachable_ask_bound(
        &self,
        schedule: Option<FeeSchedule>,
        quantity: u64,
        limit_price: Option<u128>,
    ) -> Result<u128, OrderBookError> {
        let mut remaining = quantity;
        let mut verified: u128 = 0;
        for entry in self.asks.iter() {
            let price = *entry.key();
            if limit_price.is_some_and(|limit| price > limit) {
                break;
            }
            if price > verified {
                Self::check_notional_bound(schedule, price, quantity)?;
                verified = price;
            }
            match remaining.checked_sub(entry.value().visible_quantity()) {
                Some(left) if left > 0 => remaining = left,
                // Covered by this level: nothing deeper is reachable.
                _ => break,
            }
        }
        Ok(verified)
    }

    /// Quote-notional preflight (#244): a `*_by_amount` sweep consumes at
    /// most `amount` of notional, so `amount` is its bound. Skipped when the
    /// opposite side is empty (nothing can trade).
    ///
    /// # Errors
    ///
    /// [`OrderBookError::FeeOverflow`] when a fee leg cannot price `amount`
    /// exactly.
    #[inline]
    pub(crate) fn check_amount_arithmetic(
        &self,
        side: Side,
        amount: u128,
    ) -> Result<(), OrderBookError> {
        let Some(schedule) = self.active_fee_schedule() else {
            return Ok(());
        };
        let opposite_empty = match side {
            Side::Buy => self.asks.is_empty(),
            Side::Sell => self.bids.is_empty(),
        };
        if opposite_empty {
            return Ok(());
        }
        schedule
            .check_notional(amount)
            .map_err(OrderBookError::from)
    }

    /// The configured fee schedule, or `None` when it is absent or charges
    /// nothing (a zero schedule cannot overflow).
    #[inline]
    #[must_use]
    pub(crate) fn active_fee_schedule(&self) -> Option<FeeSchedule> {
        self.fee_schedule.filter(|schedule| !schedule.is_zero_fee())
    }

    /// `price × quantity` fits `u128` and, when a schedule is active, both
    /// of its legs price it exactly (#244).
    ///
    /// # Errors
    ///
    /// [`OrderBookError::NotionalOverflow`] / [`OrderBookError::FeeOverflow`].
    #[inline]
    pub(crate) fn check_notional_bound(
        schedule: Option<FeeSchedule>,
        price: u128,
        quantity: u64,
    ) -> Result<(), OrderBookError> {
        let Some(notional) = price.checked_mul(u128::from(quantity)) else {
            return Err(OrderBookError::NotionalOverflow { price, quantity });
        };
        match schedule {
            Some(schedule) => schedule
                .check_notional(notional)
                .map_err(OrderBookError::from),
            None => Ok(()),
        }
    }

    /// [`Self::check_trade_arithmetic`] for the `match_*` / `submit_market*`
    /// entry points: a failure is recorded as a terminal
    /// `Rejected { FeeOverflow | NotionalOverflow }` with the reject metric,
    /// like every other untouched rejection. Returns the verified price the
    /// sweep seeds its backstop with.
    ///
    /// # Errors
    ///
    /// Same as [`Self::check_trade_arithmetic`].
    #[inline]
    pub(crate) fn check_trade_arithmetic_or_reject(
        &self,
        order_id: Id,
        side: Side,
        quantity: u64,
        limit_price: Option<u128>,
    ) -> Result<u128, OrderBookError> {
        self.check_trade_arithmetic(side, quantity, limit_price)
            .map_err(|err| self.reject_arithmetic_untouched(order_id, err))
    }

    /// Record a taker rejected by the trade arithmetic preflight (#244)
    /// before any mutation: terminal `Rejected` state with its reject code,
    /// the reject metric, a `WARN` line. Returns the error unchanged.
    #[cold]
    #[inline(never)]
    pub(crate) fn reject_arithmetic_untouched(
        &self,
        order_id: Id,
        err: OrderBookError,
    ) -> OrderBookError {
        let reason = crate::orderbook::reject_reason::RejectReason::from(&err);
        tracing::warn!(
            order_id = %order_id,
            error = %err,
            "taker rejected: worst-case notional or fee is not representable; book untouched"
        );
        self.track_state(
            order_id,
            crate::orderbook::order_state::OrderStatus::Rejected { reason },
        );
        crate::orderbook::metrics::record_reject(reason);
        err
    }

    /// Report committed trades whose `TradeResult` could not be built
    /// (#244). Unreachable for a sweep this book ran (see
    /// [`Self::check_trade_arithmetic`]); handled like a fold failure so the
    /// gap between the trade stream and the book is loud and counted.
    #[cold]
    #[inline(never)]
    fn report_trade_result_failure(
        &self,
        match_result: &MatchResult,
        err: &crate::orderbook::trade::TradeArithmeticError,
    ) {
        tracing::error!(
            symbol = %self.symbol,
            order_id = %match_result.order_id(),
            trade_count = match_result.trades().len(),
            error = %err,
            "committed trades could not be priced into a TradeResult; trade not published"
        );
        crate::orderbook::metrics::record_match_fold_failure();
        Self::bump_diagnostic_counter(&self.match_fold_failures, "match_fold_failures");
    }

    /// Publish a sweep's trades and resolve its outcome (#240).
    ///
    /// Emits the trade-count metric and, when a listener is installed, the
    /// `TradeResult` for every trade the sweep committed — for an aborted
    /// sweep that is the committed prefix, published exactly like a partial
    /// fill. Then returns the `MatchResult`, or the abort wrapped with that
    /// same `TradeResult` (built when `want_committed` even without a
    /// listener).
    ///
    /// # Errors
    ///
    /// [`SubmitFailure`] carrying [`OrderBookError::MatchAborted`] when the
    /// outcome was aborted.
    pub(crate) fn publish_match_outcome(
        &self,
        outcome: MatchOutcome,
        want_committed: bool,
    ) -> Result<MatchResult, SubmitFailure> {
        self.publish_match_outcome_from(outcome, want_committed, None)
    }

    /// [`Self::publish_match_outcome`] for trades whose `TradeResult`
    /// carries `origin_stop_id` (#286: the market order of an elected
    /// trailing stop).
    ///
    /// # Errors
    ///
    /// As [`Self::publish_match_outcome`].
    pub(crate) fn publish_match_outcome_from(
        &self,
        outcome: MatchOutcome,
        want_committed: bool,
        origin_stop_id: Option<Id>,
    ) -> Result<MatchResult, SubmitFailure> {
        let want_result = want_committed && outcome.aborted.is_some();
        let committed = self.publish_trades_from(&outcome.result, want_result, origin_stop_id);
        match outcome.aborted {
            // Only a `*_with_committed` caller keeps the prefix; everyone
            // else would drop it, so it is not boxed for them.
            Some(error) => Err(SubmitFailure::with_committed(
                error,
                committed.filter(|_| want_committed),
            )),
            None => Ok(outcome.result),
        }
    }

    /// Emit the trade-count metric and the trade listener for
    /// `match_result`'s trades. Returns the `TradeResult` when
    /// `want_result` is set; `None` when there were no trades or the caller
    /// did not ask for it. Every emission consumes one `engine_seq` tick.
    ///
    /// #249: the listener does not run here. Its copy is buffered in the
    /// caller's emission scope and delivered after the mutation commits and
    /// the submit gate is released. A caller that wants the result needs
    /// its `engine_seq` now, so the scope's pending events are committed at
    /// this point, followed by the trade: the sequence is the one the
    /// listener receives, minted in the same position as before.
    pub(crate) fn publish_trades(
        &self,
        match_result: &MatchResult,
        want_result: bool,
    ) -> Option<TradeResult> {
        self.publish_trades_from(match_result, want_result, None)
    }

    /// [`Self::publish_trades`] stamping `origin_stop_id` on the
    /// `TradeResult` (#286).
    pub(crate) fn publish_trades_from(
        &self,
        match_result: &MatchResult,
        want_result: bool,
        origin_stop_id: Option<Id>,
    ) -> Option<TradeResult> {
        let trade_count = match_result.trades().len();
        if trade_count == 0 {
            return None;
        }
        // The metric is independent of whether a listener is configured;
        // the `TradeResult` is only built when someone consumes it. A
        // `usize` count always fits `u64` on the supported targets; were it
        // not to, the gauge is skipped rather than fed a clamped value.
        if let Ok(trades_emitted) = u64::try_from(trade_count) {
            super::metrics::record_trades(trades_emitted);
        }
        let has_trade_listener = self.trade_listener.is_some();
        if !want_result && !has_trade_listener {
            return None;
        }
        // #244: checked notional / fee arithmetic. The preflight
        // (`check_trade_arithmetic`) and the sweep's per-level backstop keep
        // every committed trade within a notional whose fees are
        // representable, so this cannot fail for a sweep this book ran. It
        // is handled rather than assumed: the trades are already committed,
        // so a failure is reported like a fold failure (logged at `ERROR`,
        // counted by `match_fold_failures`) and no `TradeResult` carrying a
        // clamped or dropped fee is ever emitted.
        let mut trade_result = match TradeResult::with_fees(
            self.symbol.clone(),
            match_result.clone(),
            self.fee_schedule,
        ) {
            Ok(trade_result) => trade_result,
            Err(err) => {
                self.report_trade_result_failure(match_result, &err);
                return None;
            }
        };
        trade_result.origin_stop_id = origin_stop_id;
        if !want_result {
            // Listener only: buffered, stamped at commit (#249).
            self.defer_trade(trade_result);
            return None;
        }
        // #250: event stamping never affects the caller-owned result. With
        // `engine_seq` exhausted only the listener emission is suppressed
        // (logged once); the committed fills are still returned to an
        // `add_order_with_result` / `*_with_committed` caller, stamped with
        // the never-minted sentinel `u64::MAX`.
        let engine_seq = if self.has_event_listeners() {
            // Order the caller's sequence after the events this call
            // already produced (#249); the listener gets its own copy.
            let listener_copy = has_trade_listener.then(|| trade_result.clone());
            self.commit_with_trade_seq(listener_copy)
        } else {
            self.mint_event_seq()
        };
        trade_result.engine_seq = engine_seq.unwrap_or(UNSTAMPED_ENGINE_SEQ);
        Some(trade_result)
    }

    /// Match a market order specified by quote-notional amount.
    ///
    /// Walks the opposite side until the requested `amount` is consumed or
    /// the book is exhausted. This is the classic Binance-style
    /// `quoteOrderQty` semantics: callers say "buy ~$1,000 of BTC" without
    /// converting to base quantity.
    ///
    /// A level whose price the remaining notional cannot afford — one whole
    /// lot when [`Self::lot_size`] is configured, one unit otherwise — is
    /// handled according to the direction of the walk, because the levels
    /// still ahead are ordered by price:
    ///
    /// - A **buy** walks asks ascending, so every level still ahead is
    ///   dearer. An unaffordable ask ends the walk; the unspent remainder
    ///   is dust returned to the caller.
    /// - A **sell** walks bids descending, so every level still ahead is
    ///   cheaper. An unaffordable bid is **skipped** and the walk continues
    ///   to the next one, which may well be affordable. It ends only when
    ///   the budget is spent, the bid side is exhausted, or the remaining
    ///   notional is below one lot, at which point no price could fund a
    ///   lot. Selling 150 into bids of 100, 75 and 50 therefore executes one
    ///   unit at 100 and one at 50, leaving the bid at 75 untouched.
    ///
    /// The sell walk can consequently visit every level resting on the bid
    /// side. Each skipped level costs one division and mutates nothing.
    ///
    /// Bypasses STP (uses `Hash32::zero()`); use
    /// [`Self::match_market_order_by_amount_with_user`] when STP is
    /// needed.
    ///
    /// # Fees
    ///
    /// Fees are **exclusive** of `amount`. The caller pays
    /// `amount + taker_fee`; the book consumes exactly `amount` of quote
    /// liquidity (modulo any residual dust below one lot).
    ///
    /// # Returns
    ///
    /// `Ok(MatchResult)` whenever at least one transaction occurred.
    /// Inspect [`pricelevel::MatchResult::executed_value`] for the actual
    /// notional consumed; the residual dust returned to the caller is
    /// `requested - executed_value`. The accompanying `TradeResult`
    /// emitted to the trade listener carries `quote_notional` populated.
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::InsufficientLiquidityNotional`] when no
    /// trade occurred: the side is empty, or no level the walk reached was
    /// affordable. For a buy that means the best ask alone was already
    /// beyond `amount`; for a sell it means no bid on the whole side was,
    /// since the walk descends past the ones it cannot afford.
    pub fn match_market_order_by_amount(
        &self,
        order_id: Id,
        amount: u128,
        side: Side,
    ) -> Result<MatchResult, OrderBookError> {
        self.match_market_order_by_amount_with_user(order_id, amount, side, Hash32::zero())
    }

    /// Match a quote-notional market order with Self-Trade Prevention.
    ///
    /// See [`Self::match_market_order_by_amount`] for the amount / lot /
    /// fee semantics. When STP is enabled and `user_id` is non-zero, the
    /// matching engine checks resting orders for same-user conflicts
    /// before executing fills.
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::InsufficientLiquidityNotional`] when no
    /// liquidity is available, or [`OrderBookError::SelfTradePrevented`]
    /// when STP cancels the taker before any fills occur. Returns
    /// [`OrderBookError::MatchAborted`] when the sweep stopped at a price
    /// level that reported a failure (#240); the committed prefix has
    /// already reached the trade listener.
    pub fn match_market_order_by_amount_with_user(
        &self,
        order_id: Id,
        amount: u128,
        side: Side,
        user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        self.match_market_order_by_amount_committed(order_id, amount, side, user_id, false)
            .map_err(SubmitFailure::into_error)
    }

    /// Shared body of the quote-notional market sweep; see
    /// [`Self::match_market_order_committed`].
    pub(crate) fn match_market_order_by_amount_committed(
        &self,
        order_id: Id,
        amount: u128,
        side: Side,
        user_id: Hash32,
        want_committed: bool,
    ) -> Result<MatchResult, SubmitFailure> {
        trace!(
            "Order book {}: Matching notional market order {} for {} at side {:?}",
            self.symbol, order_id, amount, side
        );
        // #209: submit gate — notional market sweeps mutate the book.
        // #225: exclusive when STP is engaged for this taker, so the
        // per-level scan and the fill it authorises see the same queue. A
        // notional sweep always takes liquidity, hence `is_post_only =
        // false`.
        let _gate = self.acquire_coherent_submit_gate(
            self.submit_needs_exclusive_gate(false, user_id, false, false),
        );
        let result = (|| {
            // #240: under the gate the sweep holds, before any mutation.
            self.check_trade_id_headroom(order_id, side, None)?;
            // #244: the amount bounds the notional this sweep can consume.
            self.check_amount_arithmetic(side, amount)
                .map_err(|err| self.reject_arithmetic_untouched(order_id, err))?;
            let outcome = OrderBook::<T>::match_order_by_amount_with_user(
                self, order_id, side, amount, user_id,
            )?;
            self.publish_match_outcome(outcome, want_committed)
        })();
        // #286: under the same gate, after the taker's trades.
        self.settle_pending_stops();
        result
    }

    /// Attempts to match a limit order in the order book.
    ///
    /// This is a convenience wrapper that bypasses STP (uses `Hash32::zero()`).
    /// Use [`Self::match_limit_order_with_user`] when STP is needed.
    ///
    /// # Parameters
    /// - `order_id`: The unique identifier of the order to be matched.
    /// - `quantity`: The quantity of the order to be matched.
    /// - `side`: The side of the order book (e.g., Buy or Sell) on which the order resides.
    /// - `limit_price`: The maximum (for Buy) or minimum (for Sell) acceptable price
    ///   for the order.
    ///
    /// # Returns
    /// - `Ok(MatchResult)`: If the order is successfully matched, returning information
    ///   about the match, including possibly filled quantities and pricing details.
    /// - `Err(OrderBookError)`: If the order cannot be matched due to an error.
    pub fn match_limit_order(
        &self,
        order_id: Id,
        quantity: u64,
        side: Side,
        limit_price: u128,
    ) -> Result<MatchResult, OrderBookError> {
        self.match_limit_order_with_user(order_id, quantity, side, limit_price, Hash32::zero())
    }

    /// Attempts to match a limit order with Self-Trade Prevention support.
    ///
    /// # Arguments
    /// * `order_id` — Unique identifier for this limit order.
    /// * `quantity` — Quantity to match.
    /// * `side` — Buy or Sell.
    /// * `limit_price` — Maximum (Buy) or minimum (Sell) acceptable price.
    /// * `user_id` — Owner of the incoming order for STP checks.
    ///   Pass `Hash32::zero()` to bypass STP.
    ///
    /// # Errors
    /// Returns [`OrderBookError::SelfTradePrevented`] when STP cancels the
    /// taker before any fills occur. Returns
    /// [`OrderBookError::MatchAborted`] when the sweep stopped at a price
    /// level that reported a failure (#240); the committed prefix has
    /// already reached the trade listener.
    pub fn match_limit_order_with_user(
        &self,
        order_id: Id,
        quantity: u64,
        side: Side,
        limit_price: u128,
        user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        trace!(
            "Order book {}: Matching limit order {} for {} at side {:?} with limit price {}",
            self.symbol, order_id, quantity, side, limit_price
        );
        // #209 / #225: same gate as `match_order_with_user`. #249: trades
        // are published under it (see `match_market_order_committed`).
        let _gate = self.acquire_coherent_submit_gate(
            self.submit_needs_exclusive_gate(false, user_id, false, false),
        );
        let result = (|| {
            // #240: under the same gate the sweep holds, before any
            // mutation; only a limit that actually crosses is refused.
            self.check_trade_id_headroom(order_id, side, Some(limit_price))?;
            // #244: worst-case notional / fee representability.
            let verified =
                self.check_trade_arithmetic_or_reject(order_id, side, quantity, Some(limit_price))?;
            let outcome = self.match_order_with_user_outcome(
                order_id,
                side,
                quantity,
                Some(limit_price),
                user_id,
                TakerKind::Standard,
                SweepReservation::NONE,
                verified,
            )?;
            self.publish_match_outcome(outcome, false)
                .map_err(SubmitFailure::into_error)
        })();
        // #286: under the same gate, after the taker's trades.
        self.settle_pending_stops();
        result
    }

    /// Create a snapshot of the current order book state, up to `depth`
    /// price levels per side.
    ///
    /// # Level statistics
    ///
    /// Each level carries pricelevel's execution statistics. Captured while
    /// shared-gate takers sweep the same level they are **advisory** (may lag
    /// or be torn across fields); prices, quantities and order vectors are
    /// not affected. See [`OrderBook`]'s "Level statistics are advisory under
    /// concurrent takers" section for the exact contract.
    ///
    /// # Pending stops and last trade (#286)
    ///
    /// The snapshot also carries every pending trailing stop (not limited
    /// by `depth`, in admission order; never part of the level lists) and
    /// the last trade price, which a restore installs.
    ///
    /// # Concurrency
    ///
    /// The whole capture holds the **shared** side of the submit gate, so a
    /// call holding the exclusive side (every call that trades or touches a
    /// pending stop while one is pending, mass cancels, restores) is never
    /// captured half done: the levels, the pending stops (with their trailed
    /// terms) and the last trade price describe one state between two such
    /// calls. Shared-gate sweeps (books without pending stops) can still
    /// overlap it, with the level-statistics caveat above. It must not be
    /// called from caller code that runs under the book's gate (a `Clock`,
    /// `T::clone`): the gate is not reentrant.
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::PriceLevelError`] when a price level cannot
    /// produce a coherent snapshot (`PriceLevel::snapshot` is fallible since
    /// pricelevel 0.10, e.g. on a refused allocation or a walk that stays
    /// incoherent under concurrent mutation). No partial snapshot is returned.
    pub fn create_snapshot(&self, depth: usize) -> Result<OrderBookSnapshot, OrderBookError> {
        // Copilot / determinism review on #301: sample levels, pending
        // stops and the last trade price under one shared-gate window. A
        // bare guard: the capture emits no events.
        let _shared = self.submit_gate.read().unwrap_or_else(|poisoned| {
            self.on_submit_gate_poisoned();
            poisoned.into_inner()
        });
        // Get all bid prices and sort them in descending order
        let mut bid_prices: Vec<u128> = self.bids.iter().map(|item| *item.key()).collect();
        bid_prices.sort_by(|a, b| b.cmp(a)); // Descending order
        bid_prices.truncate(depth);

        // Get all ask prices and sort them in ascending order
        let mut ask_prices: Vec<u128> = self.asks.iter().map(|item| *item.key()).collect();
        ask_prices.sort(); // Ascending order
        ask_prices.truncate(depth);

        let mut bid_levels = Vec::with_capacity(bid_prices.len());
        let mut ask_levels = Vec::with_capacity(ask_prices.len());

        // Create snapshots for each bid level
        for price in bid_prices {
            if let Some(entry) = self.bids.get(&price) {
                bid_levels.push(entry.value().snapshot()?);
            }
        }

        // Create snapshots for each ask level
        for price in ask_prices {
            if let Some(entry) = self.asks.get(&price) {
                ask_levels.push(entry.value().snapshot()?);
            }
        }

        Ok(OrderBookSnapshot {
            symbol: self.symbol.clone(),
            timestamp: self.clock().now_millis().as_u64(),
            bids: bid_levels,
            asks: ask_levels,
            pending_stops: self.pending_stops_for_snapshot(),
            last_trade_price: self.last_trade_price(),
        })
    }

    /// Every pending trailing stop in admission order, for a snapshot
    /// (#286). Empty without `special_orders`.
    fn pending_stops_for_snapshot(&self) -> Vec<OrderType<()>> {
        #[cfg(feature = "special_orders")]
        if self.has_pending_stops() {
            return self.pending_stop_snapshot();
        }
        Vec::new()
    }

    /// Create a checksum-protected snapshot package of the entire book.
    ///
    /// The returned package includes the book's configuration fields
    /// (`fee_schedule`, `stp_mode`, `tick_size`, `lot_size`,
    /// `min_order_size`, `max_order_size`, `stop_protection`, ...) so that
    /// [`restore_from_snapshot_package`](Self::restore_from_snapshot_package)
    /// can fully reconstruct the book's state.
    ///
    /// The embedded level statistics follow [`create_snapshot`](Self::create_snapshot):
    /// a package captured while shared-gate takers sweep a level carries
    /// advisory execution statistics, and restoring it installs them
    /// verbatim. Capture with no sweep in flight when the statistics must be
    /// exact.
    pub fn create_snapshot_package(
        &self,
        depth: usize,
    ) -> Result<OrderBookSnapshotPackage, OrderBookError> {
        let snapshot = self.create_snapshot(depth)?;
        let mut package = OrderBookSnapshotPackage::new(snapshot)?;
        package.fee_schedule = self.fee_schedule;
        package.stp_mode = self.stp_mode;
        package.tick_size = self.tick_size;
        package.lot_size = self.lot_size;
        package.min_order_size = self.min_order_size;
        package.max_order_size = self.max_order_size;
        package.engine_seq = self.engine_seq();
        package.kill_switch_engaged = self.is_kill_switch_engaged();
        package.risk_config = self.risk_state.config().cloned();
        package.market_close_timestamp = self.market_close_timestamp.load(Ordering::Relaxed);
        package.has_market_close = self.has_market_close.load(Ordering::Relaxed);
        package.stop_protection = self.stop_protection;
        Ok(package)
    }

    /// Serialize a checksum-protected snapshot package to JSON.
    pub fn snapshot_to_json(&self, depth: usize) -> Result<String, OrderBookError> {
        self.create_snapshot_package(depth)?.to_json()
    }

    /// Restore the book state from a checksum-validated snapshot package.
    ///
    /// This restores both the order data and the configuration fields
    /// (`fee_schedule`, `stp_mode`, `tick_size`, `lot_size`,
    /// `min_order_size`, `max_order_size`, `engine_seq`,
    /// `kill_switch_engaged`, and the scheduled market close) that were captured by
    /// [`create_snapshot_package`](Self::create_snapshot_package), plus the
    /// last trade price and the pending trailing stops (format version 5,
    /// #286; older packages carry neither), whose risk is rebuilt with the
    /// resting orders', and the stop protection collar (format version 6,
    /// #302; older packages restore with none). The collar is installed as
    /// captured, without re-checking it against the tick size (the source
    /// book may hold one admitted under a previous tick size, see
    /// [`Self::set_stop_protection`]).
    ///
    /// The kill-switch flag is operator-driven and not journaled by
    /// the sequencer; it travels with snapshot packages only. Replay
    /// (`ReplayEngine::replay_from*`) starts a fresh book with
    /// `kill_switch_engaged = false`, and operators must engage it
    /// explicitly post-replay if needed.
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::ChecksumMismatch`] /
    /// [`OrderBookError::InvalidOperation`] when package validation
    /// fails, [`OrderBookError::EngineSeqExhausted`] when the package's
    /// `engine_seq` is `u64::MAX` (the restored book could never mint
    /// another event, #250), and every error
    /// [`restore_from_snapshot`](Self::restore_from_snapshot)
    /// documents. All of them fire before any live state is mutated
    /// (#207) — a failed package restore leaves the book, its
    /// configuration, and its risk state untouched.
    ///
    /// # Listener events
    ///
    /// A successful restore discards listener events still queued from
    /// before it (only possible after a listener panic, see
    /// [`Self::flush_listener_events`]), counting them in
    /// [`Self::dropped_listener_events`] and logging at `WARN`: they
    /// describe the replaced book and carry sequences above the restored
    /// `engine_seq`, so delivering them later would run the stream
    /// backwards. Call [`Self::flush_listener_events`] before restoring to
    /// deliver them instead.
    pub fn restore_from_snapshot_package(
        &mut self,
        package: OrderBookSnapshotPackage,
    ) -> Result<(), OrderBookError> {
        // Extract config before consuming the package via into_snapshot().
        let fee_schedule = package.fee_schedule;
        let stp_mode = package.stp_mode;
        let tick_size = package.tick_size;
        let lot_size = package.lot_size;
        let min_order_size = package.min_order_size;
        let max_order_size = package.max_order_size;
        let engine_seq = package.engine_seq;
        let kill_switch_engaged = package.kill_switch_engaged;
        let risk_config = package.risk_config.clone();
        let market_close_timestamp = package.market_close_timestamp;
        let has_market_close = package.has_market_close;
        let stop_protection = package.stop_protection;

        // Take ownership of the validated snapshot.
        let snapshot = package.into_snapshot()?;

        // #250: the package is untrusted input. A counter already at
        // `u64::MAX` could never mint another event (`next_engine_seq`
        // refuses to wrap), so the restored book would silently stop
        // publishing; reject it before any live state is touched.
        if engine_seq == u64::MAX {
            return Err(engine_seq_exhausted(engine_seq));
        }

        // Fallible phase (#207): symbol guard + level conversion + the
        // cross-level duplicate-id check all run before ANY live state —
        // book, indices, risk, config — is touched, so an invalid package
        // leaves the complete pre-restore state intact.
        self.ensure_snapshot_symbol(&snapshot)?;
        // #243: when a risk config is restored, the per-account risk
        // aggregates are computed (checked) here too, so an overflowing
        // snapshot is a typed error before any live state is touched.
        let prepared = Self::prepare_snapshot_levels(snapshot, risk_config.is_some())?;
        // #250: a crossed or locked book is malformed input.
        Self::ensure_snapshot_not_crossed(&prepared)?;

        // ---- Point of no return: everything below is infallible. ----

        // PR #289 review: the restore rewinds `engine_seq` below anything
        // still queued for the listeners (only batches a listener panic
        // left behind can be queued: `&mut self` excludes every submitter
        // and dispatcher). Those events describe the book being replaced;
        // delivering them after the restore would run the stream
        // backwards, so they are discarded and counted in
        // `dropped_listener_events`.
        let discarded = self.outbox.discard_pending();
        if discarded > 0 {
            tracing::warn!(
                symbol = %self.symbol,
                discarded,
                "snapshot restore discarded listener events queued before it (left by a listener panic)"
            );
        }

        // Preserve the persisted risk-installation state exactly:
        // install only when a config was snapshotted, otherwise
        // explicitly disable risk so a `risk_config()` call post-restore
        // returns `None` rather than `Some(empty)`. Entries and counters
        // are cleared here and replaced during the commit below by the
        // aggregates the prepare phase computed from every restored
        // resting order (#243).
        if let Some(risk_config) = risk_config {
            self.risk_state.set_config(risk_config);
        } else {
            self.risk_state.disable();
        }
        self.risk_state.clear();

        self.commit_restored_levels(&prepared);

        // Apply configuration that was captured in the package.
        self.fee_schedule = fee_schedule;
        self.stp_mode = stp_mode;
        self.tick_size = tick_size;
        self.lot_size = lot_size;
        self.min_order_size = min_order_size;
        self.max_order_size = max_order_size;
        self.set_stop_protection_unchecked(stop_protection);

        // Restore the engine's outbound monotonic counter so that the
        // first `next_engine_seq()` call on this restored book returns
        // exactly the snapshotted value, preserving cross-snapshot
        // monotonicity for downstream consumers.
        self.engine_seq.store(engine_seq, Ordering::Release);
        // #250: the restored counter was validated to be able to advance,
        // so a latched exhaustion from the pre-restore counter is cleared.
        *self.engine_seq_exhausted.get_mut() = false;

        // Restore the operational kill-switch flag so that a book
        // recovered from disaster snapshot resumes in the same
        // operational mode it was halted in.
        self.kill_switch
            .store(kill_switch_engaged, Ordering::Relaxed);

        // Restore the scheduled market close so DAY / GTD expiry resumes against the
        // same session boundary the book was snapshotted with. `restore_from_snapshot`
        // reset these to `0` / `false`, so re-apply them afterwards (#100).
        self.market_close_timestamp
            .store(market_close_timestamp, Ordering::Relaxed);
        self.has_market_close
            .store(has_market_close, Ordering::Relaxed);

        Ok(())
    }

    /// Restore the book state from a JSON payload containing a checksum-protected snapshot package.
    ///
    /// This restores both order data and configuration fields.
    /// See [`restore_from_snapshot_package`](Self::restore_from_snapshot_package).
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::DeserializationError`] on malformed
    /// JSON, plus every error
    /// [`restore_from_snapshot_package`](Self::restore_from_snapshot_package)
    /// documents — all raised before any live state is mutated (#207).
    pub fn restore_from_snapshot_json(&mut self, data: &str) -> Result<(), OrderBookError> {
        let package = OrderBookSnapshotPackage::from_json(data)?;
        self.restore_from_snapshot_package(package)
    }

    /// Restore the book state from a snapshot, without checksum validation.
    ///
    /// Rebuilds the resting bids / asks, the `order_locations` and
    /// `user_orders` indices, and — under the `special_orders` feature — the
    /// special-order tracker, so restored pegged orders resume re-pricing
    /// (#194). The snapshot's last trade price is installed and, under
    /// `special_orders`, its pending trailing stops are reinstalled in
    /// admission order with their stop price and watermark (#286); they are
    /// validated first (kind, time-in-force, quantity, consistent terms,
    /// unique ids).
    /// The rebuild uses the deterministic price-then-insertion-sequence
    /// traversal so the restore stays replay-stable (#190 / #192).
    ///
    /// # Failure atomicity
    ///
    /// The restore is two-phase (#207): every fallible step — level
    /// conversion through pricelevel's validating
    /// [`PriceLevel::from_snapshot`] and the cross-level duplicate-id
    /// check — runs against off-book structures first, and the live book
    /// is cleared and replaced only after all of them succeed. Any error
    /// therefore leaves the pre-restore bids, asks, indices, and
    /// operational state completely untouched.
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::InvalidOperation`] when the snapshot's
    /// symbol does not match this book or when one side carries two
    /// levels at the same price (the install would keep only one while
    /// the index rebuild registered both),
    /// [`OrderBookError::PriceLevelError`] when a level snapshot fails
    /// pricelevel's validation, and
    /// [`OrderBookError::DuplicateOrderId`] when the same order id
    /// appears in more than one level of the snapshot (installing it
    /// would silently orphan one of the two in `order_locations`),
    /// [`OrderBookError::QuantityOverflow`] when an order's
    /// `visible + hidden` does not fit `u64` (#250),
    /// [`OrderBookError::SnapshotCrossed`] when the snapshot's best bid is
    /// at or above its best ask (#250), and
    /// [`OrderBookError::ZeroVisibleTranche`] for a non-replenishing
    /// reserve with no visible tranche (#230).
    ///
    /// Tick / lot alignment of the restored orders is not validated: a
    /// live book legitimately keeps orders admitted under a previous tick
    /// or lot size (see [`Self::set_lot_size`]), and its snapshot must
    /// restore.
    ///
    /// # Concurrency (#225)
    ///
    /// The commit phase holds the **exclusive** side of the submit gate, so
    /// it never interleaves with an in-flight submit, cancel or modify.
    /// Listeners run after the gate is released (#249), so calling this
    /// from a listener is safe.
    pub fn restore_from_snapshot(&self, snapshot: OrderBookSnapshot) -> Result<(), OrderBookError> {
        self.ensure_snapshot_symbol(&snapshot)?;
        let prepared = Self::prepare_snapshot_levels(snapshot, false)?;
        // #250: a crossed or locked book is malformed input.
        Self::ensure_snapshot_not_crossed(&prepared)?;
        // #225: a live restore replaces every level and rebuilds the
        // `order_locations` / `user_orders` indices, so it must exclude
        // every in-flight submit, cancel and modify exactly like a
        // fill-or-kill or an STP-relevant submit does. The snapshot was
        // validated above without touching the book; only the commit
        // needs the exclusive side. `commit_restored_levels` acquires
        // nothing itself, so this is the single acquisition.
        let _gate = self.submit_gate_write();
        self.commit_restored_levels(&prepared);
        Ok(())
    }

    /// Symbol guard shared by the snapshot restore entry points.
    fn ensure_snapshot_symbol(&self, snapshot: &OrderBookSnapshot) -> Result<(), OrderBookError> {
        if snapshot.symbol != self.symbol {
            return Err(OrderBookError::InvalidOperation {
                message: format!(
                    "Snapshot symbol {} does not match order book symbol {}",
                    snapshot.symbol, self.symbol
                ),
            });
        }
        Ok(())
    }

    /// Reject a prepared snapshot that describes a crossed or locked book
    /// (#250): highest bid at or above lowest ask.
    ///
    /// A live book never rests such a pair — the later order would have
    /// matched — so the snapshot is malformed. Runs on the prepared (sorted,
    /// off-book) levels, before any live state is touched. Levels are
    /// compared as installed, empty ones included, because an installed
    /// empty level still answers `best_bid` / `best_ask`.
    ///
    /// # Errors
    ///
    /// [`OrderBookError::SnapshotCrossed`] carrying both prices.
    fn ensure_snapshot_not_crossed(
        prepared: &PreparedSnapshotLevels,
    ) -> Result<(), OrderBookError> {
        // Both vectors are sorted ascending by price.
        match (prepared.bids.last(), prepared.asks.first()) {
            (Some((best_bid, _)), Some((best_ask, _))) if best_bid >= best_ask => {
                Err(OrderBookError::SnapshotCrossed {
                    best_bid: *best_bid,
                    best_ask: *best_ask,
                })
            }
            _ => Ok(()),
        }
    }

    /// Test-only restore that skips [`Self::ensure_snapshot_not_crossed`]
    /// (#250), for regression tests of engine defences that only a crossed
    /// or locked book can reach (the residual-headroom pre-check). Every
    /// other prepare-phase check still runs. Exists only in `cfg(test)`
    /// builds.
    #[cfg(test)]
    pub(crate) fn restore_crossed_snapshot_for_test(
        &self,
        snapshot: OrderBookSnapshot,
    ) -> Result<(), OrderBookError> {
        self.ensure_snapshot_symbol(&snapshot)?;
        let prepared = Self::prepare_snapshot_levels(snapshot, false)?;
        let _gate = self.submit_gate_write();
        self.commit_restored_levels(&prepared);
        Ok(())
    }

    /// Fallible phase of a snapshot restore (#207): converts every level
    /// through pricelevel's validating [`PriceLevel::from_snapshot`] and
    /// rejects order ids that appear in more than one level — all against
    /// local structures, without touching the live book. Levels are
    /// returned sorted ascending by price so the commit phase's index
    /// rebuild keeps the deterministic price-then-insertion-sequence
    /// traversal (#192).
    ///
    /// When `rebuild_risk` is set (the package-restore path with a risk
    /// config), the per-account risk aggregates are also accumulated here
    /// with checked arithmetic (#243), so a snapshot whose open-order
    /// count, remaining quantity or resting notional is not representable
    /// fails with a typed error instead of being clamped in the commit.
    ///
    /// # Validation (#250)
    ///
    /// The snapshot is untrusted input. Per order, in the fixed traversal
    /// order, this phase rejects an id already seen at another level
    /// (`DuplicateOrderId`), a `visible + hidden` total that does not fit
    /// `u64` (`QuantityOverflow` — checked for every order since #250, not
    /// only on the risk-rebuild path), a risk aggregate overflow, and the
    /// zero-visible reserve ghost (`ZeroVisibleTranche`). Both restore entry
    /// points then run [`Self::ensure_snapshot_not_crossed`] on the result.
    ///
    /// Tick / lot alignment is deliberately **not** enforced: a live book
    /// legitimately keeps resting orders admitted under a previous tick or
    /// lot size after [`Self::set_tick_size`] / [`Self::set_lot_size`]
    /// (documented there, with the repair path), so rejecting them here
    /// would make the snapshot of a valid book fail to restore.
    fn prepare_snapshot_levels(
        snapshot: OrderBookSnapshot,
        rebuild_risk: bool,
    ) -> Result<PreparedSnapshotLevels, OrderBookError> {
        let convert = |levels: Vec<pricelevel::PriceLevelSnapshot>,
                       side: &str|
         -> Result<Vec<(u128, Arc<PriceLevel>)>, OrderBookError> {
            let mut converted = Vec::with_capacity(levels.len());
            for level_snapshot in levels {
                let price = level_snapshot.price().as_u128();
                let price_level = PriceLevel::from_snapshot(level_snapshot)
                    .map_err(OrderBookError::PriceLevelError)?;
                converted.push((price, Arc::new(price_level)));
            }
            converted.sort_by_key(|(price, _)| *price);
            // Reject duplicate prices within a side: the SkipMap install
            // would keep only the last level at that key while the index
            // rebuild below would still register the discarded level's
            // orders, leaving `order_locations` / `user_orders` pointing at
            // orders the live book does not hold. The old SkipMap-sourced
            // rebuild silently dropped one level; neither outcome is
            // acceptable, so the snapshot is rejected up front.
            let duplicate = converted.windows(2).find_map(|pair| match pair {
                [(lower, _), (upper, _)] if lower == upper => Some(*lower),
                _ => None,
            });
            if let Some(price) = duplicate {
                return Err(OrderBookError::InvalidOperation {
                    message: format!(
                        "Snapshot contains two {side} levels at the same price {price}"
                    ),
                });
            }
            Ok(converted)
        };

        let OrderBookSnapshot {
            bids,
            asks,
            pending_stops,
            last_trade_price,
            ..
        } = snapshot;
        let bids = convert(bids, "bid")?;
        let asks = convert(asks, "ask")?;

        // Cross-level duplicate-id check. Per-level duplicates are already
        // rejected by `PriceLevel::from_snapshot` (since pricelevel 0.9); an id
        // resting at two prices would silently orphan one of them in
        // `order_locations`, so reject the snapshot before any mutation.
        // A HashSet is safe here: only membership is consulted, no
        // iteration order can leak into book state.
        //
        // The same walk materializes every level's resting orders, in the
        // commit phase's replay-stable traversal order (bids ascending price,
        // then asks ascending price, each level by ascending insertion
        // sequence), into `orders`. `snapshot_by_seq_into` is fallible since
        // pricelevel 0.10; collecting here keeps that failure in the
        // fallible phase, so `commit_restored_levels` stays infallible and
        // never rebuilds indices from a stale scratch buffer.
        let mut seen: std::collections::HashSet<Id> = std::collections::HashSet::new();
        let mut orders: Vec<(u128, Side, Arc<OrderType<()>>)> = Vec::new();
        let mut level_orders: Vec<Arc<OrderType<()>>> = Vec::new();
        let mut risk = rebuild_risk.then(RiskRebuild::default);
        let sides = bids
            .iter()
            .map(|entry| (entry, Side::Buy))
            .chain(asks.iter().map(|entry| (entry, Side::Sell)));
        for ((price, level), side) in sides {
            level.snapshot_by_seq_into(&mut level_orders)?;
            orders.try_reserve(level_orders.len()).map_err(|_| {
                OrderBookError::PriceLevelError(PriceLevelError::CapacityExceeded {
                    resource: pricelevel::CapacityResource::RestoreScratch,
                    additional: level_orders.len(),
                })
            })?;
            for order in &level_orders {
                if !seen.insert(order.id()) {
                    return Err(OrderBookError::DuplicateOrderId {
                        order_id: order.id(),
                    });
                }
                // #286: a trailing stop never rests on a level. A level that
                // holds one comes from the pre-0.14 model (a stop rested as
                // a limit order at its stop price) and cannot be restored
                // faithfully: cancel it on the old version and re-submit it.
                if matches!(order.as_ref(), OrderType::TrailingStop { .. }) {
                    return Err(OrderBookError::StopOrdersUnsupported {
                        order_id: order.id(),
                    });
                }
                // #250: tranche representability is checked for every
                // order, not only when risk is rebuilt: every quantity path
                // downstream assumes an admitted order's total fits `u64`.
                let visible = order.visible_quantity().as_u64();
                let hidden = order.hidden_quantity().as_u64();
                let remaining_qty = visible
                    .checked_add(hidden)
                    .ok_or(OrderBookError::QuantityOverflow { visible, hidden })?;
                if let Some(risk) = risk.as_mut() {
                    risk.accumulate(order.id(), order.user_id(), *price, remaining_qty)?;
                }
                // #230: a legacy package can carry the one two-tranche shape
                // `pricelevel` cannot execute — a non-auto-replenishing
                // reserve with no visible tranche, which is removed without a
                // trade and strands its hidden depth at the first taker.
                // `validate_order_shape` does not run on restore, so reject
                // it here, in the prepare phase, before any book state is
                // touched: the restore fails atomically and the live book is
                // left exactly as it was.
                if let Some(hidden_quantity) = Self::is_zero_visible_ghost(order.as_ref()) {
                    return Err(OrderBookError::ZeroVisibleTranche {
                        order_id: order.id(),
                        hidden_quantity,
                    });
                }
                orders.push((*price, side, Arc::clone(order)));
            }
        }

        let stops = Self::prepare_pending_stops(pending_stops, &mut seen, risk.as_mut())?;
        // Always empty without the feature (a stop fails the prepare).
        #[cfg(not(feature = "special_orders"))]
        drop(stops);

        Ok(PreparedSnapshotLevels {
            bids,
            asks,
            orders,
            risk,
            #[cfg(feature = "special_orders")]
            stops,
            last_trade_price,
        })
    }

    /// Validates a snapshot's pending trailing stops (#286), in the
    /// fallible phase of a restore.
    ///
    /// Each must be a trailing stop with a positive quantity and a
    /// time-in-force that can pend (`GTC`, `GTD`, `DAY`), consistent terms
    /// (positive trail, stop price not beyond its watermark, which trailing
    /// preserves) and an id no resting order or other stop uses. A stop the
    /// snapshot's last trade price crosses is legal (elections are
    /// suspended while the kill switch is engaged): stops are only
    /// evaluated against new trades, so it is elected by the next print at
    /// or through its stop price. Their risk contribution is accumulated at
    /// their stop price. Without `special_orders` any pending stop fails
    /// with [`OrderBookError::StopOrdersUnsupported`].
    ///
    /// # Errors
    ///
    /// [`OrderBookError::StopOrdersUnsupported`],
    /// [`OrderBookError::InvalidStopTerms`],
    /// [`OrderBookError::DuplicateOrderId`], the risk aggregate overflows of
    /// [`RiskRebuild::accumulate`], or [`OrderBookError::InvalidOperation`]
    /// naming the malformed stop.
    fn prepare_pending_stops(
        pending_stops: Vec<OrderType<()>>,
        seen: &mut std::collections::HashSet<Id>,
        mut risk: Option<&mut RiskRebuild>,
    ) -> Result<Vec<OrderType<()>>, OrderBookError> {
        #[cfg(not(feature = "special_orders"))]
        {
            let _ = (seen, risk.as_mut());
            match pending_stops.first() {
                Some(stop) => Err(OrderBookError::StopOrdersUnsupported {
                    order_id: stop.id(),
                }),
                None => Ok(pending_stops),
            }
        }
        #[cfg(feature = "special_orders")]
        {
            use super::stop_orders::StopTerms;
            let malformed = |order_id: Id, what: &str| OrderBookError::InvalidOperation {
                message: format!("snapshot pending stop {order_id} {what}"),
            };
            for stop in &pending_stops {
                let order_id = stop.id();
                let Some(terms) = StopTerms::of(stop) else {
                    return Err(malformed(order_id, "is not a trailing stop"));
                };
                if stop.is_immediate() {
                    return Err(malformed(order_id, "has an immediate time-in-force"));
                }
                let quantity = stop.visible_quantity().as_u64();
                if quantity == 0 {
                    return Err(malformed(order_id, "has no quantity"));
                }
                if let Some(reason) = terms.inconsistency() {
                    return Err(OrderBookError::InvalidStopTerms { order_id, reason });
                }
                if !seen.insert(order_id) {
                    return Err(OrderBookError::DuplicateOrderId { order_id });
                }
                if let Some(risk) = risk.as_deref_mut() {
                    risk.accumulate(order_id, stop.user_id(), terms.stop, quantity)?;
                }
            }
            Ok(pending_stops)
        }
    }

    /// Infallible commit phase of a snapshot restore (#207): clears the
    /// live book and installs the pre-validated levels, then rebuilds the
    /// `order_locations` and `user_orders` indices, the special-order
    /// tracker (under `special_orders`), and — when the prepare phase
    /// computed risk aggregates (the package-restore path with a risk
    /// config) — the per-account risk entries and counters, installed in
    /// one shot from the checked [`RiskRebuild`] (#243).
    ///
    /// The index rebuild runs in one fixed, replay-stable pass: bids
    /// ascending price then asks ascending price (the prepared vectors are
    /// pre-sorted), and within each level ascending insertion sequence as
    /// materialized by [`PriceLevel::snapshot_by_seq_into`] in the prepare
    /// phase (`PreparedSnapshotLevels::orders`), never the `DashMap`-backed
    /// `iter_orders` view, whose per-instance-hashed order would leak into
    /// the `user_orders` admission sequence and make a subsequent
    /// `cancel_orders_by_user` diverge across restores of the same package
    /// (#192). This is NOT the original admission history — a snapshot
    /// cannot recover that — but it is deterministic. `order_locations`,
    /// the tracker's `DashSet`s, and the risk maps are order-insensitive,
    /// so their rebuild order does not leak; only `user_orders`, whose
    /// per-user admission order is consumed by `cancel_orders_by_user`, needs
    /// the fixed traversal. Every level read happened in the prepare phase,
    /// so no fallible pricelevel call remains here.
    fn commit_restored_levels(&self, prepared: &PreparedSnapshotLevels) {
        self.cache.invalidate();

        // Clear all existing data
        while let Some(entry) = self.bids.pop_front() {
            drop(entry);
        }
        while let Some(entry) = self.asks.pop_front() {
            drop(entry);
        }
        self.order_locations.clear();
        self.user_orders.clear();
        // The special-order tracker is a full replacement on restore: clear it
        // here and rebuild it below from the restored resting orders, mirroring
        // the `user_orders` / `order_locations` rebuild (#194).
        #[cfg(feature = "special_orders")]
        {
            self.special_order_tracker.clear();
            self.pending_stops.clear();
        }
        // #286: the last trade price travels with the snapshot (format
        // version 5; `None` for older ones), because pending stops trail
        // and trigger on it.
        match prepared.last_trade_price {
            Some(price) => {
                self.last_trade_price.store(price);
                self.has_traded.store(true, Ordering::Relaxed);
            }
            None => {
                self.has_traded.store(false, Ordering::Relaxed);
                self.last_trade_price.store(0);
            }
        }
        // #230: recounted from the orders installed below, like every other
        // index this commit rebuilds. Reset here so a restore cannot inherit
        // the pre-restore book's strandable makers.
        self.strandable_makers_resting.store(0, Ordering::Relaxed);
        self.has_market_close.store(false, Ordering::Relaxed);
        self.market_close_timestamp.store(0, Ordering::Relaxed);

        for (price, level) in &prepared.bids {
            self.bids.insert(*price, level.clone());
        }
        for (price, level) in &prepared.asks {
            self.asks.insert(*price, level.clone());
        }

        // The orders were materialized by the prepare phase in the fixed
        // traversal order (bids then asks, ascending price, ascending
        // insertion sequence), so this walk performs no fallible call.
        for (price, side, order) in &prepared.orders {
            let (price, side) = (*price, *side);
            let user_seq = self.track_user_order(order.user_id(), order.id());
            self.order_locations.insert(
                order.id(),
                OrderLocation::new(price, side, order.user_id(), user_seq),
            );
            // #230: the count is not carried by the snapshot; it is
            // recounted from what the restore actually installs, so
            // a restored non-auto reserve keeps its discard
            // reportable and a restore that installs none closes the
            // gate.
            self.note_rested_order(order.as_ref());
            #[cfg(feature = "special_orders")]
            self.reregister_special_order(order.as_ref());
        }
        // #286: the pending stops, validated in the prepare phase, in
        // admission order (sequences 0..n keep their relative priority).
        #[cfg(feature = "special_orders")]
        self.install_pending_stops(&prepared.stops);
        // #243: the risk aggregates were accumulated with checked
        // arithmetic in the prepare phase; installing them cannot fail.
        if let Some(risk) = prepared.risk.as_ref() {
            self.risk_state.install_rebuild(risk);
        }
    }

    /// Installs a restore's validated pending stops (#286) with admission
    /// sequences `0..n`, in order, into the cleared store.
    #[cfg(feature = "special_orders")]
    fn install_pending_stops(&self, stops: &[OrderType<()>]) {
        let mut seq: u64 = 0;
        for stop in stops {
            if let Err(err) = self.pending_stops.insert_at(*stop, seq) {
                // Unreachable: the prepare phase validated kind and ids.
                tracing::error!(
                    symbol = %self.symbol,
                    order_id = %stop.id(),
                    error = %err,
                    "restore could not install a validated pending stop"
                );
            }
            match seq.checked_add(1) {
                Some(next) => seq = next,
                None => break,
            }
        }
        self.pending_stops.set_next_seq(seq);
    }

    /// Is `order` the one two-tranche shape `pricelevel` cannot execute
    /// (#230): a [`OrderType::ReserveOrder`] with `auto_replenish == false`,
    /// no visible tranche and hidden quantity behind it?
    ///
    /// `match_against` returns `(0, None, 0, remaining)` for it — no trade,
    /// and the maker is removed with its whole hidden tranche stranded — so
    /// it is a ghost that must never rest. The single source of truth for
    /// that shape, shared by `validate_order_shape` (admission and every
    /// modify projection) and by the snapshot-restore prepare phase, which
    /// deliberately does **not** run the full admission validator.
    ///
    /// The sibling shapes are executable and are not covered: a zero-visible
    /// iceberg draws its whole hidden tranche into visible on match, and a
    /// zero-visible auto-replenishing reserve refreshes and re-queues.
    #[inline]
    #[must_use]
    pub(super) fn is_zero_visible_ghost<E>(order: &OrderType<E>) -> Option<u64> {
        match order {
            OrderType::ReserveOrder {
                visible_quantity,
                hidden_quantity,
                auto_replenish: false,
                ..
            } if visible_quantity.as_u64() == 0 && hidden_quantity.as_u64() > 0 => {
                Some(hidden_quantity.as_u64())
            }
            _ => None,
        }
    }

    /// Is `order` a maker that will strand hidden quantity if a sweep
    /// exhausts its visible tranche (#230)? That is exactly a
    /// [`OrderType::ReserveOrder`] with `auto_replenish == false` and hidden
    /// depth behind it: `pricelevel` removes it on depletion instead of
    /// refreshing, dropping the hidden tranche.
    #[inline]
    #[must_use]
    pub(super) fn is_strandable_maker<E>(order: &OrderType<E>) -> bool {
        matches!(
            order,
            OrderType::ReserveOrder {
                hidden_quantity,
                auto_replenish: false,
                ..
            } if hidden_quantity.as_u64() > 0
        )
    }

    /// Count `order` in if it is a strandable maker, on the way onto a
    /// level.
    ///
    /// Called from the only two paths that rest an order: the level
    /// insertion in `add_order_inner` and the snapshot-restore commit. See
    /// [`Self::strandable_makers_resting`] for why those two make the count
    /// exact.
    ///
    /// Checked (#250): the count is bounded by the number of resting
    /// orders, so it cannot reach `usize::MAX`; were it to, the increment
    /// is refused and logged at `WARN` instead of wrapping to zero (which
    /// would close the strandable-maker gate while such makers rest).
    #[inline]
    pub(super) fn note_rested_order<E>(&self, order: &OrderType<E>) {
        if Self::is_strandable_maker(order)
            && self
                .strandable_makers_resting
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    current.checked_add(1)
                })
                .is_err()
        {
            Self::warn_strandable_count("increment overflow at usize::MAX");
        }
    }

    /// Count one strandable maker out, by the checked rule
    /// [`Self::note_removed_order`] also uses, when the caller has already
    /// established that the maker leaving the level is one.
    ///
    /// Used by the fill drain in `match_order_inner`, which identifies the
    /// maker from that sweep's own capture list rather than from an
    /// `OrderType` it still holds. Single implementation of the checked
    /// decrement, so the two removal shapes cannot drift.
    ///
    /// Checked (#250): a decrement at zero means a removal was reported
    /// for a maker the count never saw. It is refused (the count stays at
    /// zero, never wraps to `usize::MAX`) and logged at `WARN`, since it
    /// signals an accounting bug rather than a reachable state.
    #[inline]
    pub(super) fn note_removed_strandable_maker(&self) {
        if self
            .strandable_makers_resting
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_sub(1)
            })
            .is_err()
        {
            Self::warn_strandable_count("decrement underflow at zero");
        }
    }

    /// Log a refused strandable-maker count update (#250) out of line.
    #[cold]
    #[inline(never)]
    fn warn_strandable_count(reason: &'static str) {
        tracing::warn!(
            counter = "strandable_makers_resting",
            reason,
            "strandable-maker count update refused; count left unchanged"
        );
    }

    /// Reset the strandable-maker count to zero.
    ///
    /// For the two paths that empty the book wholesale rather than removing
    /// orders one at a time: the bulk `cancel_all_orders`, and the
    /// snapshot-restore commit, which then recounts from the orders it
    /// installs. Both leave nothing resting at the moment they call this, so
    /// zero is the exact count, not an approximation.
    #[inline]
    pub(super) fn reset_strandable_makers(&self) {
        self.strandable_makers_resting.store(0, Ordering::Relaxed);
    }

    /// Count `order` out if it is a strandable maker, on the way off a
    /// level.
    ///
    /// Called from the only three paths that remove one:
    /// `cancel_order_with_reason`, `cancel_resting_maker_on_level` and the
    /// fill drain in `match_order_inner`. Checked, so a spurious call can
    /// never wrap the count below zero; see
    /// [`Self::strandable_makers_resting`].
    #[inline]
    pub(super) fn note_removed_order<E>(&self, order: &OrderType<E>) {
        if Self::is_strandable_maker(order) {
            self.note_removed_strandable_maker();
        }
    }

    /// Re-register a restored resting order with the special-order tracker
    /// when it is a pegged order.
    ///
    /// Mirrors the admission-time registration in
    /// [`add_order`](Self::add_order) so restored pegged orders resume
    /// re-pricing after a snapshot restore (#194). Called once per restored
    /// resting order from the deterministic price-then-sequence rebuild pass
    /// in [`restore_from_snapshot`](Self::restore_from_snapshot), so any
    /// tracker mutation stays replay-stable. The tracker holds only order
    /// ids; the pegged price lives in the order data and survives the
    /// snapshot round-trip. Trailing stops are not tracked here: they are
    /// pending off-book stops the snapshot carries itself (#286).
    #[cfg(feature = "special_orders")]
    #[inline]
    fn reregister_special_order(&self, order: &OrderType<()>) {
        if let OrderType::PeggedOrder { id, .. } = order {
            self.special_order_tracker.register_pegged_order(*id);
        }
    }

    /// Creates an enriched snapshot with pre-calculated metrics
    ///
    /// This provides better performance than creating a snapshot and calculating
    /// metrics separately, as it computes everything in a single pass through the data.
    /// All metrics are calculated by default.
    ///
    /// # Arguments
    /// - `depth`: Maximum number of price levels to include on each side
    ///
    /// # Returns
    /// `EnrichedSnapshot` with all metrics pre-calculated
    ///
    /// The metrics are computed from prices and quantities only. The
    /// embedded level snapshots carry execution statistics that are advisory
    /// under concurrent takers, as for [`create_snapshot`](Self::create_snapshot).
    ///
    /// # Performance
    /// O(N) where N is depth, single pass through data for all metrics.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 101, 10, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// let snapshot = book.enriched_snapshot(10)?;
    ///
    /// if let Some(mid) = snapshot.mid_price {
    ///     println!("Mid price: {}", mid);
    /// }
    /// if let Some(spread) = snapshot.spread_bps {
    ///     println!("Spread: {} bps", spread);
    /// }
    /// println!("Bid depth: {}", snapshot.bid_depth_total);
    /// println!("Imbalance: {}", snapshot.order_book_imbalance);
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::PriceLevelError`] when a price level cannot
    /// produce a coherent snapshot (`PriceLevel::snapshot` is fallible since
    /// pricelevel 0.10) or when a level's `visible + hidden` total overflows
    /// `u64`, and [`OrderBookError::ArithmeticOverflow`] when a selected
    /// metric's depth (`u64`) or VWAP notional (`u128`) aggregate overflows
    /// (#245). No partial snapshot is returned.
    pub fn enriched_snapshot(&self, depth: usize) -> Result<EnrichedSnapshot, OrderBookError> {
        self.enriched_snapshot_with_metrics(depth, MetricFlags::ALL)
    }

    /// Creates an enriched snapshot with custom metric selection
    ///
    /// Allows you to specify which metrics to calculate for optimization.
    /// Only the selected metrics will be computed, others will have default values.
    ///
    /// # Arguments
    /// - `depth`: Maximum number of price levels to include on each side
    /// - `flags`: Bitflags specifying which metrics to calculate
    ///
    /// # Returns
    /// `EnrichedSnapshot` with selected metrics calculated. As for
    /// [`enriched_snapshot`](Self::enriched_snapshot), the embedded level
    /// statistics are advisory under concurrent takers.
    ///
    /// # Errors
    ///
    /// Returns [`OrderBookError::PriceLevelError`] when a price level cannot
    /// produce a coherent snapshot (`PriceLevel::snapshot` is fallible since
    /// pricelevel 0.10) or when a level's `visible + hidden` total overflows
    /// `u64`, and [`OrderBookError::ArithmeticOverflow`] when a selected
    /// metric's depth (`u64`) or VWAP notional (`u128`) aggregate overflows
    /// (#245). No partial snapshot is returned.
    ///
    /// # Performance
    /// O(N) where N is depth, but faster than `enriched_snapshot()` if fewer metrics selected.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::{OrderBook, MetricFlags};
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 101, 10, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// // Calculate only mid price and spread for performance
    /// let snapshot = book.enriched_snapshot_with_metrics(
    ///     10,
    ///     MetricFlags::MID_PRICE | MetricFlags::SPREAD
    /// )?;
    ///
    /// assert!(snapshot.mid_price.is_some());
    /// assert!(snapshot.spread_bps.is_some());
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn enriched_snapshot_with_metrics(
        &self,
        depth: usize,
        flags: MetricFlags,
    ) -> Result<EnrichedSnapshot, OrderBookError> {
        // The SkipMap is already price-ordered, so iterate it directly and take
        // only the requested depth — no collect/sort/truncate of all keys and no
        // redundant second lookup per kept level. Bids are highest-first (reverse
        // iteration); asks are lowest-first.
        let bid_levels: Vec<PriceLevelSnapshot> = self
            .bids
            .iter()
            .rev()
            .take(depth)
            .map(|entry| entry.value().snapshot())
            .collect::<Result<_, PriceLevelError>>()?;

        let ask_levels: Vec<PriceLevelSnapshot> = self
            .asks
            .iter()
            .take(depth)
            .map(|entry| entry.value().snapshot())
            .collect::<Result<_, PriceLevelError>>()?;

        // Create enriched snapshot with pre-calculated metrics
        EnrichedSnapshot::with_metrics(
            self.symbol.clone(),
            self.clock().now_millis().as_u64(),
            bid_levels,
            ask_levels,
            depth, // Use depth for VWAP calculation
            depth, // Use depth for imbalance calculation
            flags,
        )
    }

    /// Get the total volume (`visible + hidden`, in quantity units) at each
    /// price level, as `(bid_volumes, ask_volumes)` keyed by price.
    ///
    /// # Errors
    /// Returns [`OrderBookError::PriceLevelError`] when a level's
    /// `visible + hidden` total overflows `u64` (previously read as `0`).
    #[allow(clippy::type_complexity)]
    pub fn get_volume_by_price(
        &self,
    ) -> Result<(HashMap<u128, u64>, HashMap<u128, u64>), OrderBookError> {
        let mut bid_volumes = HashMap::new();
        let mut ask_volumes = HashMap::new();

        // Calculate bid volumes
        for item in self.bids.iter() {
            bid_volumes.insert(*item.key(), level_total(item.value())?);
        }

        // Calculate ask volumes
        for item in self.asks.iter() {
            ask_volumes.insert(*item.key(), level_total(item.value())?);
        }

        Ok((bid_volumes, ask_volumes))
    }

    /// Get a BTreeMap of bids with price as key and PriceLevel as value
    ///
    /// # Errors
    /// Returns [`OrderBookError::PriceLevelError`] if a level cannot produce
    /// a coherent snapshot (pricelevel 0.10) or if rebuilding a level from
    /// its snapshot fails validation (pricelevel validates snapshot
    /// admission instead of trusting it).
    pub fn get_bt_bids(&self) -> Result<BTreeMap<u128, PriceLevel>, OrderBookError> {
        self.bids
            .iter()
            .map(|entry| {
                let price = *entry.key();
                let snapshot = entry.value().snapshot()?;
                let price_level = PriceLevel::try_from(&snapshot)?;
                Ok((price, price_level))
            })
            .collect()
    }

    /// Get a BTreeMap of asks with price as key and PriceLevel as value
    ///
    /// # Errors
    /// Returns [`OrderBookError::PriceLevelError`] if a level cannot produce
    /// a coherent snapshot (pricelevel 0.10) or if rebuilding a level from
    /// its snapshot fails validation (pricelevel validates snapshot
    /// admission instead of trusting it).
    pub fn get_bt_asks(&self) -> Result<BTreeMap<u128, PriceLevel>, OrderBookError> {
        self.asks
            .iter()
            .map(|entry| {
                let price = *entry.key();
                let snapshot = entry.value().snapshot()?;
                let price_level = PriceLevel::try_from(&snapshot)?;
                Ok((price, price_level))
            })
            .collect()
    }

    /// Get a fresh copy of the order-location index, wrapped in an `Arc`.
    ///
    /// This is **not** a handle to the live map: the index is cloned
    /// entry-by-entry and the `Arc` owns the copy, so later admissions,
    /// cancels and modifies on this book are not reflected in it and writes
    /// to it do not reach the book. Treat the result as a point-in-time
    /// snapshot of `order id -> (price, side)`, and re-read it when a
    /// current view is needed. Cost is proportional to the number of
    /// resting orders.
    #[must_use]
    pub fn get_order_locations_arc(&self) -> Arc<DashMap<Id, (u128, Side)>> {
        Arc::new(
            self.order_locations
                .iter()
                .map(|entry| (*entry.key(), entry.value().price_side()))
                .collect(),
        )
    }

    /// Computes comprehensive depth statistics for a side of the order book
    ///
    /// Analyzes the top N price levels to provide detailed statistical metrics
    /// about liquidity distribution, including volume, average sizes, weighted
    /// prices, and variability measures.
    ///
    /// # Arguments
    /// - `side`: The side to analyze (Buy for bids, Sell for asks)
    /// - `levels`: Maximum number of top levels to analyze (0 = all levels)
    ///
    /// # Returns
    /// `DepthStats` containing comprehensive statistics. Returns zero stats if no levels exist.
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when an analyzed level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when the total volume
    ///   (`u64`) or the price-weighted volume (`u128`) overflows.
    ///
    /// # Performance
    /// O(N) where N is the number of levels analyzed.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 99, 20, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 98, 30, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// let stats = book.depth_statistics(Side::Buy, 10)?;
    /// println!("Total volume: {}", stats.total_volume);
    /// println!("Average level size: {:.2}", stats.avg_level_size);
    /// println!("Weighted avg price: {:.2}", stats.weighted_avg_price);
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn depth_statistics(
        &self,
        side: Side,
        levels: usize,
    ) -> Result<DepthStats, OrderBookError> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return Ok(DepthStats::zero());
        }

        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter().rev()),
            Side::Sell => Either::Right(price_levels.iter()),
        };

        let mut total_volume = 0u64;
        let mut weighted_price_sum = 0u128;
        let mut sizes = Vec::new();
        let mut min_size = u64::MAX;
        let mut max_size = 0u64;
        let mut count = 0usize;

        for entry in iter {
            if levels > 0 && count >= levels {
                break;
            }

            let price = *entry.key();
            let quantity = level_total(entry.value())?;

            if quantity == 0 {
                continue;
            }

            total_volume = checked_depth_add(total_volume, quantity, "depth statistics volume")?;
            weighted_price_sum = checked_notional_add(
                weighted_price_sum,
                price,
                quantity,
                "depth statistics weighted price",
            )?;
            sizes.push(quantity);
            min_size = min_size.min(quantity);
            max_size = max_size.max(quantity);
            count = count
                .checked_add(1)
                .ok_or_else(|| analytics_overflow("depth statistics level count"))?;
        }

        if count == 0 || total_volume == 0 {
            return Ok(DepthStats::zero());
        }

        let avg_level_size = total_volume as f64 / count as f64;
        let weighted_avg_price = weighted_price_sum as f64 / total_volume as f64;

        // Calculate standard deviation
        let variance: f64 = sizes
            .iter()
            .map(|&size| {
                let diff = size as f64 - avg_level_size;
                diff * diff
            })
            .sum::<f64>()
            / count as f64;
        let std_dev = variance.sqrt();

        Ok(DepthStats {
            total_volume,
            levels_count: count,
            avg_level_size,
            weighted_avg_price,
            min_level_size: if min_size == u64::MAX { 0 } else { min_size },
            max_level_size: max_size,
            std_dev_level_size: std_dev,
        })
    }

    /// Calculates buy and sell pressure based on total volume on each side
    ///
    /// Returns the total quantity on the bid and ask sides as a measure
    /// of market pressure. Higher values indicate stronger interest.
    ///
    /// # Returns
    /// Tuple of `(buy_pressure, sell_pressure)` where each value is the total
    /// quantity available on that side (in units).
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when a side's total overflows
    ///   `u64`.
    ///
    /// # Performance
    /// O(N + M) where N is bid levels and M is ask levels.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 50, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 101, 30, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// let (buy_pressure, sell_pressure) = book.buy_sell_pressure()?;
    /// println!("Buy: {}, Sell: {}", buy_pressure, sell_pressure);
    ///
    /// if buy_pressure > sell_pressure {
    ///     println!("More buying interest");
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn buy_sell_pressure(&self) -> Result<(u64, u64), OrderBookError> {
        let buy_pressure = self.bids.iter().try_fold(0u64, |acc, entry| {
            checked_depth_add(acc, level_total(entry.value())?, "buy pressure")
        })?;

        let sell_pressure = self.asks.iter().try_fold(0u64, |acc, entry| {
            checked_depth_add(acc, level_total(entry.value())?, "sell pressure")
        })?;

        Ok((buy_pressure, sell_pressure))
    }

    /// Detects if the order book is thin (has low liquidity)
    ///
    /// A thin book has insufficient liquidity, which can lead to high slippage
    /// and price volatility. This method checks if the total volume in the top
    /// N levels falls below a threshold.
    ///
    /// # Arguments
    /// - `threshold`: Minimum total volume required (in units)
    /// - `levels`: Number of top levels to check on each side
    ///
    /// # Returns
    /// `true` if either side has insufficient liquidity, `false` otherwise
    ///
    /// # Errors
    /// Propagates [`Self::depth_statistics`] errors
    /// ([`OrderBookError::PriceLevelError`],
    /// [`OrderBookError::ArithmeticOverflow`]).
    ///
    /// # Performance
    /// O(N) where N is levels to check.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 100, 5, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), 101, 5, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// if book.is_thin_book(100, 5)? {
    ///     println!("Warning: Thin book detected - high slippage risk!");
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn is_thin_book(&self, threshold: u64, levels: usize) -> Result<bool, OrderBookError> {
        let bid_stats = self.depth_statistics(Side::Buy, levels)?;
        let ask_stats = self.depth_statistics(Side::Sell, levels)?;

        Ok(bid_stats.total_volume < threshold || ask_stats.total_volume < threshold)
    }

    /// Calculates depth distribution histogram for a side
    ///
    /// Divides the order book depth into equal price bins and calculates
    /// the total volume in each bin. Useful for visualizing liquidity
    /// distribution and identifying concentration points.
    ///
    /// # Arguments
    /// - `side`: The side to analyze (Buy for bids, Sell for asks)
    /// - `bins`: Number of bins to divide the depth into (must be > 0).
    ///   Capped at [`MAX_DEPTH_DISTRIBUTION_BINS`]: a larger request builds
    ///   exactly `MAX_DEPTH_DISTRIBUTION_BINS` bins, so the result length is
    ///   `min(bins, MAX_DEPTH_DISTRIBUTION_BINS)`.
    ///
    /// # Returns
    /// Vector of `DistributionBin` containing price ranges and volumes.
    /// Returns an empty vector if bins is 0 or no levels exist. Bins are
    /// contiguous and never inverted (`min_price <= max_price`, so
    /// [`DistributionBin::width`] is always `Ok`). Each bin's `max_price` is
    /// exclusive; the last bin ends at `max_level_price + 1`. When `bins`
    /// exceeds the observed price span, the surplus trailing bins are empty
    /// zero-width bins at `max_level_price + 1`.
    ///
    /// # Errors
    /// - [`OrderBookError::PriceLevelError`] when a level's
    ///   `visible + hidden` total overflows `u64`.
    /// - [`OrderBookError::ArithmeticOverflow`] when a bin bound leaves the
    ///   `u128` price domain (for example a level at `u128::MAX`, whose
    ///   exclusive upper bound `u128::MAX + 1` is not representable) or a
    ///   bin's volume / level count overflows.
    /// - [`OrderBookError::AllocationFailed`] when the (capped) bin vector
    ///   cannot be reserved.
    ///
    /// # Performance
    /// O(N + B) where N is the total number of levels and B the bin count.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    /// use uuid::Uuid;
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// for i in 0..10 {
    ///     let price = 100 - i;
    ///     let _ = book.add_limit_order(Id::from_uuid(Uuid::new_v4()), price, 10, Side::Buy, TimeInForce::Gtc, None);
    /// }
    ///
    /// let distribution = book.depth_distribution(Side::Buy, 5)?;
    /// for bin in distribution {
    ///     println!("Price {}-{}: {} units in {} levels",
    ///              bin.min_price, bin.max_price, bin.volume, bin.level_count);
    /// }
    /// # Ok::<(), orderbook_rs::OrderBookError>(())
    /// ```
    pub fn depth_distribution(
        &self,
        side: Side,
        bins: usize,
    ) -> Result<Vec<DistributionBin>, OrderBookError> {
        let bins = bins.min(MAX_DEPTH_DISTRIBUTION_BINS);
        if bins == 0 {
            return Ok(Vec::new());
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        // The SkipMap is price-ordered: the extremes are its two ends.
        let (Some(front), Some(back)) = (price_levels.front(), price_levels.back()) else {
            return Ok(Vec::new());
        };
        let min_price = *front.key();
        let max_price = *back.key();

        // Calculate bin width (ceiling division). A concurrent insert between
        // the two end reads can only widen the observed range, never invert
        // it; an inverted read is reported rather than clamped.
        let price_range = max_price
            .checked_sub(min_price)
            .ok_or_else(|| analytics_overflow("depth distribution price range"))?;
        // `bins >= 1` and `usize` fits `u128` on every supported target.
        let bins_u128 = bins as u128;
        let bin_width = if price_range == 0 {
            1
        } else {
            price_range.div_ceil(bins_u128)
        };
        let last_index = bins
            .checked_sub(1)
            .ok_or_else(|| analytics_overflow("depth distribution last bin"))?;

        // Initialize bins
        let mut distribution: Vec<DistributionBin> = Vec::new();
        distribution
            .try_reserve_exact(bins)
            .map_err(|_| OrderBookError::AllocationFailed {
                operation: "depth distribution bins",
                requested: bins,
            })?;
        // Exclusive upper bound of the whole histogram: the last bin is
        // inclusive of the highest level. Every bin bound is clamped to it,
        // so when `bins` exceeds the observed price span the surplus bins
        // are empty, zero-width `[upper, upper)` bins instead of running
        // past the range (and never inverted).
        let upper = max_price
            .checked_add(1)
            .ok_or_else(|| analytics_overflow("depth distribution bin bound"))?;
        let mut bin_min = min_price;
        for i in 0..bins {
            let bin_max = if i == last_index {
                upper
            } else {
                // A `bin_min + bin_width` that overflows `u128` is past
                // `upper` too, so clamping it to `upper` is exact.
                bin_min
                    .checked_add(bin_width)
                    .map_or(upper, |end| end.min(upper))
            };

            distribution.push(DistributionBin {
                min_price: bin_min,
                max_price: bin_max,
                volume: 0,
                level_count: 0,
            });

            if i != last_index {
                // Bins are contiguous: the next one starts where this ends.
                bin_min = bin_max;
            }
        }

        // Fill bins with data. Only the observed `[min_price, max_price]`
        // band is binned: a level inserted concurrently outside it after the
        // two end reads is not part of this histogram.
        for entry in price_levels.range(min_price..=max_price) {
            let price = *entry.key();
            let quantity = level_total(entry.value())?;

            if quantity == 0 {
                continue;
            }

            // Find which bin this price belongs to (`bin_width >= 1`).
            let raw_index = price
                .checked_sub(min_price)
                .and_then(|offset| offset.checked_div(bin_width))
                .ok_or_else(|| analytics_overflow("depth distribution bin index"))?;
            let bin_index =
                usize::try_from(raw_index).map_or(last_index, |index| index.min(last_index));

            let bin = distribution
                .get_mut(bin_index)
                .ok_or_else(|| analytics_overflow("depth distribution bin index"))?;
            bin.volume = checked_depth_add(bin.volume, quantity, "depth distribution bin volume")?;
            bin.level_count = bin
                .level_count
                .checked_add(1)
                .ok_or_else(|| analytics_overflow("depth distribution bin level count"))?;
        }

        Ok(distribution)
    }
}

/// Build the [`OrderBookError::EngineSeqExhausted`] error out of line
/// (#250).
#[cold]
#[inline(never)]
#[must_use]
fn engine_seq_exhausted(engine_seq: u64) -> OrderBookError {
    OrderBookError::EngineSeqExhausted { engine_seq }
}

// Implementation of RepricingOperations trait for OrderBook
#[cfg(feature = "special_orders")]
use crate::orderbook::repricing::{RepricingOperations, RepricingResult, calculate_pegged_price};

#[cfg(feature = "special_orders")]
impl<T> OrderBook<T>
where
    T: Clone + Default + Send + Sync + 'static,
{
    /// Whether no admission owns `order_id` (#291).
    ///
    /// The location entry is the id's ownership token (#288): an order
    /// claims it before its level admits it, registers its special-order
    /// tracking after the admission, and every remover unregisters the
    /// tracking before it releases the location. A repricer that found no
    /// order for a tracked id therefore releases the entry only while the id
    /// is unowned; a same-id order admitted after `get_order` returned
    /// `None` owns the id (and possibly already the entry, whose insert was
    /// a no-op), so its registration is kept. Checked under the tracker's
    /// shard lock (see `SpecialOrderTracker::unregister_pegged_order_if`).
    #[inline]
    fn id_unowned(&self, order_id: Id) -> bool {
        !self.order_locations.contains_key(&order_id)
    }

    /// Re-price every pegged order, returning the count repriced and pushing a
    /// `(order_id, reason)` pair onto `failures` for each one whose
    /// `update_order` is **rejected** (e.g. a risk-admission rejection). The
    /// validate-first modify (#98/#168) means a rejected re-price leaves the
    /// order at its old price — but it was previously swallowed by
    /// `if ...is_ok()`; now it is surfaced (#174). A peg that simply has no
    /// reference / no valid passive tick this cycle is a no-op, not a failure,
    /// and is not recorded.
    ///
    /// Each re-price goes through the public, gated `update_order`, so on an
    /// STP-enabled book this loop takes and releases the exclusive submit
    /// gate once per order (#225). Correct and deadlock-free — the gate is
    /// never held across iterations — but the sweep is not atomic as a
    /// batch: concurrent flow interleaves between consecutive re-prices.
    ///
    /// # Errors
    ///
    /// [`OrderBookError::ArithmeticOverflow`] if the repriced count cannot
    /// be incremented (#250). The count is bounded by the tracker's id
    /// list, so this is unreachable; it is checked rather than assumed.
    fn reprice_pegged_collecting(
        &self,
        failures: &mut Vec<(Id, String)>,
    ) -> Result<usize, OrderBookError> {
        let pegged_ids = self.special_order_tracker.pegged_order_ids();
        if pegged_ids.is_empty() {
            return Ok(0);
        }

        let best_bid = self.best_bid();
        let best_ask = self.best_ask();
        // Exact integer midpoint (#245): no `f64` round trip / truncating cast.
        let mid_price = self.integer_mid_price();
        let last_trade = if self.has_traded.load(Ordering::Relaxed) {
            Some(self.last_trade_price.load())
        } else {
            None
        };

        let mut repriced_count = 0;

        for order_id in pegged_ids {
            if let Some(order) = self.get_order(order_id) {
                if let OrderType::PeggedOrder {
                    price: current_price,
                    side,
                    reference_price_offset,
                    reference_price_type,
                    ..
                } = order.as_ref()
                    && let Some(new_price) = calculate_pegged_price(
                        *reference_price_type,
                        *reference_price_offset,
                        *side,
                        best_bid,
                        best_ask,
                        mid_price,
                        last_trade,
                        self.tick_size,
                    )
                    && new_price != current_price.as_u128()
                {
                    let update = OrderUpdate::UpdatePrice {
                        order_id,
                        new_price: pricelevel::Price::new(new_price),
                    };
                    match self.update_order(update) {
                        Ok(_) => {
                            repriced_count = checked_reprice_count(repriced_count)?;
                            trace!(
                                "Re-priced pegged order {} from {} to {}",
                                order_id, current_price, new_price
                            );
                        }
                        Err(e) => {
                            trace!(
                                "Pegged re-price of {} to {} rejected: {}",
                                order_id, new_price, e
                            );
                            failures.push((
                                order_id,
                                format!("pegged re-price to {new_price} rejected: {e}"),
                            ));
                        }
                    }
                }
            } else {
                #[cfg(test)]
                if let Some(hook) = self.reprice_interleave_hook.as_ref() {
                    hook(self, order_id);
                }
                // No order rests under this id: release the stale
                // registration, unless an order claimed the id meanwhile
                // (#291, see `id_unowned`).
                self.special_order_tracker
                    .unregister_pegged_order_if(&order_id, || self.id_unowned(order_id));
            }
        }

        Ok(repriced_count)
    }

    /// Evaluate the pending trailing stops against any print not evaluated
    /// yet (#286) under the exclusive submit gate, returning how many stop
    /// prices trailed.
    ///
    /// Every call that trades evaluates its own prints before it returns,
    /// so this finds nothing to do and returns `0`; it exists for API
    /// compatibility. Returns `0` without touching the gate when no stop
    /// is pending.
    fn reprice_trailing_collecting(&self) -> Result<usize, OrderBookError> {
        // Hot-path review P2-04: nothing to evaluate, and no reason to
        // serialize the book behind an exclusive acquisition.
        if !self.has_pending_stops() {
            return Ok(0);
        }
        let _gate = self.acquire_coherent_submit_gate(true);
        Ok(self.run_stop_triggers().trailed)
    }
}

/// Increment a repricing sweep's repriced-order count with checked
/// arithmetic (#250).
#[cfg(feature = "special_orders")]
#[inline]
fn checked_reprice_count(count: usize) -> Result<usize, OrderBookError> {
    count
        .checked_add(1)
        .ok_or(OrderBookError::ArithmeticOverflow {
            operation: "repriced order count",
        })
}

#[cfg(feature = "special_orders")]
impl<T> RepricingOperations<T> for OrderBook<T>
where
    T: Clone + Default + Send + Sync + 'static,
{
    /// Re-prices all pegged orders based on current market conditions.
    ///
    /// Returns the number repriced. Per-order re-price *failures* are not
    /// surfaced through this count-only entry point — use
    /// [`reprice_special_orders`](Self::reprice_special_orders), whose
    /// [`RepricingResult::failed_orders`] records every rejected re-price.
    fn reprice_pegged_orders(&self) -> Result<usize, OrderBookError> {
        self.reprice_pegged_collecting(&mut Vec::new())
    }

    /// Evaluates the pending trailing stops against the last trade price
    /// under the exclusive submit gate (#286) and returns how many stop
    /// prices trailed; elected stops execute. Stops are evaluated after
    /// every trade anyway, so this normally returns `0`.
    fn reprice_trailing_stops(&self) -> Result<usize, OrderBookError> {
        self.reprice_trailing_collecting()
    }

    /// Re-prices the pegged orders, evaluates the pending trailing stops
    /// (see [`reprice_trailing_stops`](Self::reprice_trailing_stops)) and
    /// reports per-order failures.
    ///
    /// [`RepricingResult::failed_orders`] is populated with a `(order_id,
    /// reason)` pair for every re-price whose `update_order` was rejected (e.g.
    /// a risk-admission rejection). These were previously swallowed (#174); a
    /// rejected re-price leaves the order at its prior price (validate-first
    /// modify, #98/#168).
    fn reprice_special_orders(&self) -> Result<RepricingResult, OrderBookError> {
        let mut failed_orders = Vec::new();
        let pegged_count = self.reprice_pegged_collecting(&mut failed_orders)?;
        let trailing_count = self.reprice_trailing_collecting()?;

        Ok(RepricingResult {
            pegged_orders_repriced: pegged_count,
            trailing_stops_repriced: trailing_count,
            failed_orders,
        })
    }

    /// Checks if a trailing stop order should be triggered
    fn should_trigger_trailing_stop(
        &self,
        order: &OrderType<T>,
        current_market_price: u128,
    ) -> bool {
        if let OrderType::TrailingStop {
            price: stop_price,
            side,
            ..
        } = order
        {
            match side {
                // Sell trailing stop triggers when market falls to or below stop price
                Side::Sell => current_market_price <= stop_price.as_u128(),
                // Buy trailing stop triggers when market rises to or above stop price
                Side::Buy => current_market_price >= stop_price.as_u128(),
            }
        } else {
            false
        }
    }
}

#[cfg(feature = "special_orders")]
impl<T> OrderBook<T>
where
    T: Clone + Default + Send + Sync + 'static,
{
    /// Returns the number of tracked pegged orders
    pub fn pegged_order_count(&self) -> usize {
        self.special_order_tracker.pegged_order_count()
    }

    /// Number of pending trailing stops (#286).
    pub fn trailing_stop_count(&self) -> usize {
        self.pending_stops.len()
    }

    /// Returns all tracked pegged order IDs
    pub fn pegged_order_ids(&self) -> Vec<Id> {
        self.special_order_tracker.pegged_order_ids()
    }

    /// Ids of the pending trailing stops, in admission order (#286).
    pub fn trailing_stop_ids(&self) -> Vec<Id> {
        self.pending_stop_ids(|_| true)
    }
}

/// Off-book output of the fallible phase of a snapshot restore (#207):
/// fully converted price levels, sorted ascending by price per side, ready
/// for the infallible commit phase
/// ([`OrderBook::commit_restored_levels`]). Holding these outside the book
/// is what makes a failed restore leave the live book untouched.
struct PreparedSnapshotLevels {
    /// `(price, level)` pairs for the bid side, ascending by price.
    bids: Vec<(u128, Arc<PriceLevel>)>,
    /// `(price, level)` pairs for the ask side, ascending by price.
    asks: Vec<(u128, Arc<PriceLevel>)>,
    /// Every resting order of `bids` then `asks`, as `(price, side, order)`,
    /// in the deterministic index-rebuild order: ascending price per side,
    /// ascending insertion sequence within a level. Materialized in the
    /// fallible prepare phase so the commit phase performs no fallible
    /// level read.
    orders: Vec<(u128, Side, Arc<OrderType<()>>)>,
    /// Checked per-account risk aggregates for the restored resting
    /// orders and pending stops (#243, #286); `Some` only on the
    /// package-restore path with a risk config.
    risk: Option<RiskRebuild>,
    /// Validated pending trailing stops, in admission order (#286).
    #[cfg(feature = "special_orders")]
    stops: Vec<OrderType<()>>,
    /// The snapshot's last trade price (#286).
    last_trade_price: Option<u128>,
}

/// Number of [`OrderBook::level_locks`] stripes (#247). A power of two
/// large enough that two unrelated prices rarely share one.
pub(super) const LEVEL_LOCK_STRIPES: usize = 64;

/// Test-only hook fired inside a cancel-then-add modify (#247); see
/// `OrderBook::modify_interleave_hook`.
#[cfg(test)]
pub(super) type ModifyInterleaveHook<T> =
    std::sync::Arc<dyn Fn(&OrderBook<T>, Id, super::modifications::ModifyPhase) + Send + Sync>;

/// Test-only hook fired inside a repricer (#291); see
/// `OrderBook::reprice_interleave_hook`.
#[cfg(all(test, feature = "special_orders"))]
pub(super) type RepriceInterleaveHook<T> = std::sync::Arc<dyn Fn(&OrderBook<T>, Id) + Send + Sync>;

/// Test-only failure injected into a single-order cancel (#248); see
/// `OrderBook::cancel_fault_hook`.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(super) enum CancelFault {
    /// The level refuses the removal; nothing is mutated.
    Refuse(pricelevel::PriceLevelError),
    /// The level removes the order, then reports this failure.
    RemoveThenFail(pricelevel::PriceLevelError),
}

/// Guard over the submit gate (#209 / #225) in either mode — held for the
/// length of one mutating entry-point call.
/// [`OrderBook::acquire_coherent_submit_gate`] is the single place that picks the
/// mode and carries the full rationale.
///
/// # Listener emission (#249)
///
/// On a book with any listener installed the guard also owns the call's
/// emission scope: every trade, price-level and order-state event the call
/// produces is buffered instead of being delivered inline. Dropping the
/// guard, in this order:
///
/// 1. commits the buffered events to the book's outbox while the gate is
///    **still held**, stamping `engine_seq` there, so outbox order is both
///    sequence order and commit order;
/// 2. releases the gate;
/// 3. marks the batch ready and dispatches (or leaves it to the thread
///    already dispatching).
///
/// Listeners therefore never run under the gate or mid-mutation, and a
/// listener may re-enter the book. On a book with no listener the guard
/// opens no scope and dropping it only releases the gate.
///
/// # Unwinding (#294)
///
/// When the guard is dropped by a panic that started after it was taken
/// (`std::thread::panicking()`, checked once per drop, exactly as std's
/// own poison flag does), the book engages its kill switch and latches
/// [`OrderBook::submit_gate_poisoned`] **before** the gate is released, on
/// the shared side as well as the exclusive one: a `RwLockReadGuard`
/// never poisons, so this is the only signal an unwind under the shared
/// side leaves. Nothing is dispatched (see [`OrderBook::submit_gate_read`]).
pub(super) struct SubmitGateGuard<'a, T> {
    /// The book whose gate is held.
    book: &'a OrderBook<T>,
    /// The held side of the gate; `Released` once dropped.
    lock: GateLock<'a>,
    /// The call's emission scope, when a listener is installed.
    emission: Option<super::emission::GateEmission>,
    /// `true` when the thread was already unwinding when the gate was
    /// taken (a destructor running during an unrelated panic called the
    /// book). A second panic inside the critical section would abort the
    /// process, so an unwind seen at drop is then not this call's.
    panicking_on_entry: bool,
}

/// The held side of the submit gate. Only the drop timing matters, hence
/// the `_`-prefixed fields.
pub(super) enum GateLock<'a> {
    /// Shared mode: everything whose decision does not span two operations
    /// — ordinary and post-only submits, `UpdateQuantity`, `Cancel`, every
    /// modify on an `STPMode::None` book, cancels and anonymous match-only
    /// sweeps.
    Read {
        /// Held for its drop only.
        _guard: std::sync::RwLockReadGuard<'a, ()>,
    },
    /// Exclusive mode: a fill-or-kill submit's feasibility + sweep window
    /// (#209); an STP-relevant submit's per-level scan + fill window and
    /// the matching-capable modifies (`UpdatePrice`,
    /// `UpdatePriceAndQuantity`, `Replace`) that carry the same window
    /// under STP; every mass cancel and expiry eviction (#248); and the
    /// live snapshot restore commit (#225).
    Write {
        /// Held for its drop only.
        _guard: std::sync::RwLockWriteGuard<'a, ()>,
    },
    /// The gate has been released.
    Released,
}

impl<'a, T> SubmitGateGuard<'a, T> {
    /// Wrap a held gate side, opening the emission scope when the book has
    /// a listener installed.
    #[inline]
    fn new(book: &'a OrderBook<T>, lock: GateLock<'a>) -> Self {
        let emission = super::emission::GateEmission::open(book);
        Self {
            book,
            lock,
            emission,
            panicking_on_entry: std::thread::panicking(),
        }
    }
}

impl<T> SubmitGateGuard<'_, T> {
    /// `true` while the shared side is held.
    #[inline]
    fn is_shared(&self) -> bool {
        matches!(self.lock, GateLock::Read { .. })
    }

    /// The held side's name for the poison log, `None` once released.
    #[inline]
    fn held_side(&self) -> Option<&'static str> {
        match self.lock {
            GateLock::Read { .. } => Some("shared"),
            GateLock::Write { .. } => Some("exclusive"),
            GateLock::Released => None,
        }
    }
}

/// Unwind sentinel for the commit phase of [`SubmitGateGuard`]'s drop
/// (#294, PR #297 review).
///
/// The commit runs with the gate still held but **inside** the guard's
/// drop, and can run caller code (the `tracing` subscriber, e.g. the
/// engine-sequence exhaustion or refused-allocation logs). A panic there
/// does not run the guard's drop again, and a shared read guard leaves no
/// poison, so without this sentinel the kill switch and latch would stay
/// unset. Armed only when the drop started on a non-unwinding thread, and
/// disarmed after the gate is released; dropped while the thread panics,
/// it latches before the gate's own field drop releases it.
struct CommitSentinel<'a, T> {
    /// The book whose gate is held.
    book: &'a OrderBook<T>,
    /// The held side, `None` when disarmed.
    side: Option<&'static str>,
}

impl<T> Drop for CommitSentinel<'_, T> {
    fn drop(&mut self) {
        if let Some(side) = self.side
            && std::thread::panicking()
        {
            self.book.latch_submit_gate_unwind(side);
        }
    }
}

impl<T> Drop for SubmitGateGuard<'_, T> {
    fn drop(&mut self) {
        // #294: an unwind through the held gate, on either side. Engaged
        // before the gate is released below, so the next holder sees it.
        let unwinding = std::thread::panicking();
        if unwinding
            && !self.panicking_on_entry
            && let Some(gate_side) = self.held_side()
        {
            self.book.latch_submit_gate_unwind(gate_side);
        }
        let Some(emission) = self.emission.take() else {
            // No listener: the gate is released by the field drop.
            return;
        };
        // PR #297 review: a panic raised by the commit itself (caller
        // `tracing` code under the held gate) is caught by this sentinel.
        let mut sentinel = CommitSentinel {
            book: self.book,
            side: if unwinding { None } else { self.held_side() },
        };
        // 1. Commit under the gate (no-op while unwinding, see
        //    `GateEmission::commit`).
        let committed = emission.commit(self.book);
        // 2. Release the gate before any listener runs.
        self.lock = GateLock::Released;
        sentinel.side = None;
        drop(sentinel);
        // 3. Deliver.
        match committed {
            super::emission::Committed::Nothing => {}
            super::emission::Committed::Queued(ticket) => {
                self.book.release_and_dispatch(ticket);
            }
            super::emission::Committed::Direct(events) => self.book.deliver_direct(events),
        }
    }
}
