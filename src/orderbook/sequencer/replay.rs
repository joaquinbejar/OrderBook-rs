//! Deterministic replay engine for event journals.
//!
//! [`ReplayEngine`] reads a sequence of [`SequencerEvent`]s from a [`Journal`]
//! and re-applies each command to a fresh [`OrderBook`], producing an
//! identical final state. This enables disaster recovery, audit compliance,
//! and state verification.
//!
//! # Concurrent same-id submits are not replayable
//!
//! Replay is sequential, so it reproduces a journal recorded from one
//! submitting thread (as a sequencer feeds the book), not every interleaving
//! of a book driven concurrently on the shared submit gate. One case is
//! called out because it involves trades: two concurrent submits carrying
//! the same fresh order id can both pass the early duplicate check, both
//! sweep, and only one can then rest (the id is claimed atomically, #288);
//! the loser returns `DuplicateOrderId` **after its trades**. The journal
//! records only the reject code (the error does not carry the fills), and
//! replayed in journal order the loser meets the winner already resting and
//! is refused by the early check with no fills. The codes agree, so replay
//! does not stop there: the replayed book simply lacks the loser's trades,
//! a divergence only [`snapshots_match`] (or a later outcome mismatch)
//! reveals. Assigning unique order ids, or serializing submits per id, is
//! an ingress / sequencing obligation; the engine only guarantees that the
//! live book stays consistent (one resting order per id, no orphaned index).

use super::error::JournalError;
use super::journal::Journal;
use super::types::{CommittedPrefix, SequencerCommand, SequencerEvent, SequencerResult};
use crate::orderbook::clock::Clock;
use crate::orderbook::fees::FeeSchedule;
use crate::orderbook::mass_cancel::{MassCancelFailure, MassCancelResult};
use crate::orderbook::reject_reason::RejectReason;
use crate::orderbook::stop_protection::StopProtection;
use crate::orderbook::stp::STPMode;
use crate::orderbook::trade::SubmitFailure;
use crate::orderbook::{OrderBook, OrderBookError, OrderBookSnapshot};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

/// Book configuration injected into a fresh [`OrderBook`] before replay so
/// that a journal produced by a **non-default-config** book reconstructs to
/// the same structure.
///
/// The plain [`ReplayEngine::replay_from`] / [`ReplayEngine::replay_from_with_clock`]
/// entry points build the target book with all configuration left at its
/// defaults (`tick_size` / `lot_size` / `min_order_size` / `max_order_size`
/// = `None`, `stp_mode` = [`STPMode::None`], `fee_schedule` = `None`). A book
/// that used any of these — for example a `MarketOrderByAmount` that rounds
/// per level under a `lot_size`, a self-cross prevented live by STP, or fees —
/// would replay into a **structurally different** book, so `snapshots_match`
/// can fail at verify and the recovered state would be wrong.
///
/// To recover such a book deterministically, carry the original configuration
/// alongside the journal (it is the same set of fields persisted in
/// [`OrderBookSnapshotPackage`](crate::OrderBookSnapshot)'s package form) and
/// replay through a `*_with_config` variant
/// ([`ReplayEngine::replay_from_with_config`] /
/// [`ReplayEngine::replay_from_with_clock_and_config`]).
///
/// The configuration is supplied by the **caller** — replay does not read it
/// from the journal, so the on-disk journal format is unchanged and
/// `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` is not bumped.
///
/// Beyond the structural fields, the config can also carry the source book's
/// **trade-ID namespace** (`trade_id_namespace`, see
/// [`OrderBook::set_trade_id_namespace`]): with it, a **full** `*_with_config`
/// replay (`from_sequence == 0`) under an injected [`Clock`] reproduces the
/// live run's trade-ID stream byte-identically, not just its structure.
/// Suffix replays with a namespace are rejected
/// ([`ReplayError::NamespaceRequiresFullReplay`]) because the restarted ID
/// counter would mint wrong or duplicate IDs. Without a namespace the fresh
/// book keeps a random one and replayed trade IDs differ from the live ones.
///
/// `Default` yields the all-defaults configuration, equivalent to the plain
/// replay entry points.
#[derive(Debug, Clone, Default)]
pub struct ReplayBookConfig {
    /// Fee schedule the source book used, or `None` for no fees. Applied via
    /// [`OrderBook::set_fee_schedule`].
    ///
    /// Must match the source book exactly: since #244 the fee schedule
    /// affects verdicts, not only the fees on the trades. A taker whose
    /// worst-case notional cannot be priced under the schedule is rejected
    /// with `RejectReason::FeeOverflow` (code 18), so replaying with a
    /// different schedule can turn a recorded fill into a rejection (or the
    /// reverse) and stops with [`ReplayError::OutcomeMismatch`].
    pub fee_schedule: Option<FeeSchedule>,

    /// Self-trade prevention mode the source book used. [`STPMode::None`]
    /// (the default) disables STP. Applied via [`OrderBook::set_stp_mode`].
    pub stp_mode: STPMode,

    /// Tick size (minimum price increment) the source book used, or `None`
    /// for no tick validation. Applied via [`OrderBook::set_tick_size_opt`].
    pub tick_size: Option<u128>,

    /// Lot size (minimum quantity increment) the source book used, or `None`
    /// for no lot validation / rounding. Applied via
    /// [`OrderBook::set_lot_size_opt`].
    pub lot_size: Option<u64>,

    /// Minimum order size the source book used, or `None` for no minimum.
    /// Applied via [`OrderBook::set_min_order_size`] only when `Some`.
    pub min_order_size: Option<u64>,

    /// Maximum order size the source book used, or `None` for no maximum.
    /// Applied via [`OrderBook::set_max_order_size`] only when `Some`.
    pub max_order_size: Option<u64>,

    /// Trade-ID namespace the source book used, or `None` to keep the fresh
    /// book's random namespace. Applied via
    /// [`OrderBook::set_trade_id_namespace`] only when `Some`, before any
    /// journal events are replayed (the fresh book has no orders, so the
    /// generator's counter-restart contract is satisfied by construction).
    /// Inject the live book's namespace (#199) to make the replayed trade-ID
    /// stream byte-identical to the original.
    ///
    /// Because applying a namespace restarts the trade-ID counter at 0, a
    /// namespace-carrying config is only valid for a **full replay**: the
    /// `*_with_config` entry points reject `from_sequence != 0` with
    /// [`ReplayError::NamespaceRequiresFullReplay`] rather than minting
    /// wrong or duplicate IDs for a suffix. The journal must also cover the
    /// trade-ID stream origin — a rotated segment whose earlier segments
    /// already produced trades under this namespace reissues their IDs,
    /// which the engine cannot detect.
    pub trade_id_namespace: Option<Uuid>,

    /// Protection collar for elected stop orders the source book used
    /// (#302), or `None` for unprotected (market) execution. Applied as
    /// is (no tick-size re-check: the source book may hold a collar
    /// admitted under a previous tick size). Chain
    /// [`Self::with_stop_protection`] to set it.
    ///
    /// A collar changes which trades an elected stop makes, so it must
    /// match the source book **and** have been constant over the replayed
    /// range: `set_stop_protection` is not journaled, so a collar changed
    /// mid-stream cannot be reproduced.
    ///
    /// A mismatch is **not** reported as [`ReplayError::OutcomeMismatch`]
    /// at the election (stop elections are not journaled commands, and the
    /// command that printed still succeeds), and [`snapshots_match`] only
    /// catches it through its effects: [`OrderBookSnapshot`] does not carry
    /// the collar, so the snapshots differ only when the mismatch changed
    /// an outcome (a stop elected and filled differently). If no stop was
    /// elected over the replayed range, or the fills happened to coincide,
    /// the snapshots match while the two books would elect future stops
    /// differently. Verify the configuration explicitly: compare the
    /// replayed book's
    /// [`OrderBook::stop_protection`] with the source book's (or with the
    /// snapshot package's
    /// [`stop_protection`](crate::orderbook::OrderBookSnapshotPackage::stop_protection)
    /// field).
    pub stop_protection: Option<StopProtection>,
}

impl ReplayBookConfig {
    /// Creates a [`ReplayBookConfig`] from its six structural fields.
    ///
    /// Equivalent to building the struct with public-field syntax; provided so
    /// callers can construct the carrier without naming every field at the call
    /// site. Use [`ReplayBookConfig::default`] for the all-defaults case. The
    /// `trade_id_namespace` field defaults to `None` — chain
    /// [`Self::with_trade_id_namespace`] to set it.
    ///
    /// # Arguments
    ///
    /// * `fee_schedule` — fee schedule the source book used, or `None`
    /// * `stp_mode` — self-trade prevention mode the source book used
    /// * `tick_size` — tick size the source book used, or `None`
    /// * `lot_size` — lot size the source book used, or `None`
    /// * `min_order_size` — minimum order size the source book used, or `None`
    /// * `max_order_size` — maximum order size the source book used, or `None`
    #[must_use]
    pub fn new(
        fee_schedule: Option<FeeSchedule>,
        stp_mode: STPMode,
        tick_size: Option<u128>,
        lot_size: Option<u64>,
        min_order_size: Option<u64>,
        max_order_size: Option<u64>,
    ) -> Self {
        Self {
            fee_schedule,
            stp_mode,
            tick_size,
            lot_size,
            min_order_size,
            max_order_size,
            trade_id_namespace: None,
            stop_protection: None,
        }
    }

    /// Returns this configuration with the trade-ID namespace set.
    ///
    /// Builder-style companion to [`Self::new`] (which leaves the namespace
    /// at `None`): carry the live book's namespace (#199) so a **full**
    /// `*_with_config` replay (`from_sequence == 0`) under an injected
    /// [`Clock`] reproduces the live trade-ID stream byte-identically.
    /// Suffix replays with a namespace are rejected — see
    /// [`ReplayError::NamespaceRequiresFullReplay`].
    ///
    /// # Arguments
    ///
    /// * `namespace` — trade-ID namespace the source book used
    #[must_use = "with_trade_id_namespace returns the updated config; it does not mutate in place"]
    pub fn with_trade_id_namespace(mut self, namespace: Uuid) -> Self {
        self.trade_id_namespace = Some(namespace);
        self
    }

    /// Returns this configuration with the stop protection collar set
    /// (#302).
    ///
    /// Builder-style companion to [`Self::new`] (which leaves it at
    /// `None`): carry the source book's
    /// [`OrderBook::stop_protection`] so replayed stop elections trade
    /// within the same band.
    ///
    /// # Arguments
    ///
    /// * `protection` — the collar the source book used, or `None`
    #[must_use = "with_stop_protection returns the updated config; it does not mutate in place"]
    pub fn with_stop_protection(mut self, protection: Option<StopProtection>) -> Self {
        self.stop_protection = protection;
        self
    }

    /// Applies this configuration to a freshly-constructed `book` in place,
    /// before any journal events are replayed into it.
    ///
    /// `fee_schedule`, `stp_mode`, `tick_size`, and `lot_size` are applied
    /// unconditionally (a `None` / [`STPMode::None`] value resets the field to
    /// its default, which is a no-op on a fresh book). `min_order_size` and
    /// `max_order_size` are applied only when `Some`, mirroring the existing
    /// `set_min_order_size` / `set_max_order_size` setters which take a bare
    /// value rather than an `Option`. `trade_id_namespace` is applied only
    /// when `Some` — the book is fresh (no orders yet), so replacing the
    /// generator here honors the counter-restart contract of
    /// [`OrderBook::set_trade_id_namespace`]. `stop_protection` is applied
    /// unconditionally, without the tick-size check.
    fn apply_to<T>(&self, book: &mut OrderBook<T>)
    where
        T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Default + 'static,
    {
        book.set_fee_schedule(self.fee_schedule);
        book.set_stp_mode(self.stp_mode);
        book.set_tick_size_opt(self.tick_size);
        book.set_lot_size_opt(self.lot_size);
        if let Some(min) = self.min_order_size {
            book.set_min_order_size(min);
        }
        if let Some(max) = self.max_order_size {
            book.set_max_order_size(max);
        }
        if let Some(namespace) = self.trade_id_namespace {
            book.set_trade_id_namespace(namespace);
        }
        book.set_stop_protection_unchecked(self.stop_protection);
    }
}

/// Errors that can occur during journal replay.
///
/// `#[non_exhaustive]` since 0.14.0 (#260): new failure modes are added over
/// time (0.14 added `JournalTruncated` and `MassCancelMismatch`), so a match
/// outside this crate needs a wildcard arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// The journal contains no events to replay.
    #[error("journal is empty — nothing to replay")]
    EmptyJournal,

    /// The requested starting sequence number exceeds the journal's last entry.
    #[error("invalid from_sequence {from_sequence}: journal last sequence is {last_sequence}")]
    InvalidSequence {
        /// The sequence number requested.
        from_sequence: u64,
        /// The last sequence number in the journal.
        last_sequence: u64,
    },

    /// A gap was detected between expected and found sequence numbers.
    #[error("sequence gap detected: expected {expected}, found {found}")]
    SequenceGap {
        /// The expected next sequence number.
        expected: u64,
        /// The actual sequence number found.
        found: u64,
    },

    /// The journal's entries ended before the last sequence it reports
    /// (#295).
    ///
    /// Replay reads [`Journal::last_sequence`] before iterating and
    /// requires the iteration to reach it. An iterator that stops short (for
    /// example a `FileJournal` whose newest segment file vanished from the
    /// directory) would otherwise return `Ok` on a truncated prefix,
    /// indistinguishable from a complete replay.
    #[error(
        "journal truncated: last sequence is {expected_last}, but replay reached {}",
        describe_reached(*.reached)
    )]
    JournalTruncated {
        /// The last sequence the journal reported before replay started.
        expected_last: u64,
        /// The last sequence replay read, or `None` when it read no entry.
        reached: Option<u64>,
    },

    /// The protocol sequence counter overflowed `u64` while advancing.
    ///
    /// Unreachable at any realistic journal length, but advancing the counter
    /// with a checked add (rather than a saturating one) keeps gap detection
    /// correct at the boundary instead of silently stalling `expected_seq`.
    #[error("replay sequence counter overflowed u64 at sequence {at}")]
    SequenceOverflow {
        /// The sequence number that could not be advanced past.
        at: u64,
    },

    /// An OrderBook operation failed during replay.
    #[error("order book error during replay at sequence {sequence_num}: {source}")]
    OrderBookError {
        /// The sequence number of the event that caused the error.
        sequence_num: u64,
        /// The underlying error.
        #[source]
        source: OrderBookError,
    },

    /// A re-executed submit reached a different verdict than the journal
    /// recorded.
    ///
    /// Raised for an `AddOrder` / `MarketOrder` / `MarketOrderByAmount`
    /// event journaled as [`SequencerResult::RejectedWithCode`] whose
    /// re-execution succeeded, or failed under a different
    /// [`RejectReason`]. Replay applies such a rejection by re-executing
    /// it, because the live command may have traded before it failed; the
    /// reproduced fills are only faithful if the re-execution fails the
    /// same way, so a disagreement means the reconstructed book has
    /// diverged from the live one — typically a [`ReplayBookConfig`] that
    /// does not match the source book, a journal that does not start at
    /// the book's origin, or state the journal cannot carry (a market
    /// order's user identity).
    ///
    /// Also raised for an event journaled as
    /// [`SequencerResult::MatchAborted`] (#240) whose re-execution does not
    /// abort with the recorded committed prefix: it succeeded, failed under
    /// another code, or aborted after committing different fills (`actual`
    /// then carries the replayed `MatchAborted`, and the first divergent
    /// trade is logged at `ERROR`). An update journaled as
    /// `RejectedWithCode` under [`RejectReason::MatchAborted`] is
    /// re-executed and reconciled by code the same way.
    #[error(
        "replay diverged at sequence {sequence_num}: journal recorded rejection `{recorded}`, replay {}",
        describe_outcome(.actual)
    )]
    OutcomeMismatch {
        /// The sequence number of the event whose verdict disagreed.
        sequence_num: u64,
        /// The reject code the journal recorded.
        recorded: RejectReason,
        /// What the re-execution produced: `None` when it succeeded,
        /// `Some(err)` when it failed under a different code.
        actual: Option<OrderBookError>,
    },

    /// The journal recorded a self-trade-prevention rejection under a
    /// different [`STPMode`] than the replay book is configured with.
    ///
    /// `OrderBookError::SelfTradePrevented` carries the mode that decided
    /// the verdict, so a journaled STP rejection pins the source book's
    /// mode at that point in the stream. Two modes can refuse the same
    /// taker under the same [`RejectReason::SelfTradePrevention`] and still
    /// leave different books behind — `CancelTaker` leaves the same-user
    /// maker resting where `CancelBoth` cancels it — so reconciling the
    /// reject code cannot catch the difference. Replay checks the recorded
    /// mode against its own before dispatching the event and refuses to
    /// continue, rather than returning a book that quietly differs from
    /// the live one.
    ///
    /// Fix the [`ReplayBookConfig`] to carry the source book's
    /// `stp_mode`. The check is a lower bound, not full coverage: it can
    /// only fire on journals that recorded at least one STP rejection with
    /// its mode, so a mode difference in a run that never prevented a
    /// self-trade is still invisible to it.
    #[error(
        "replay configuration mismatch at sequence {sequence_num}: the journal recorded a self-trade-prevention rejection under STP mode `{recorded}`, but the replay book uses `{actual}`"
    )]
    StpModeMismatch {
        /// The sequence number of the event carrying the recorded mode.
        sequence_num: u64,
        /// The STP mode the source book used, as the journal recorded it.
        recorded: STPMode,
        /// The STP mode the replay book is configured with.
        actual: STPMode,
    },

    /// A trade-ID namespace was injected for a suffix replay.
    ///
    /// [`OrderBook::set_trade_id_namespace`] restarts the UUID v5 counter at
    /// 0, so the byte-identical trade-ID guarantee only holds when replay
    /// starts at the origin of the trade-ID stream. A namespace-carrying
    /// [`ReplayBookConfig`] combined with a non-zero `from_sequence` would
    /// mint IDs from counter 0 — wrong for the suffix and duplicates of IDs
    /// already emitted live under that namespace — so the `*_with_config`
    /// entry points reject the combination instead. Replay from sequence 0,
    /// or drop the namespace from the config for suffix replays.
    #[error(
        "trade-ID namespace injection requires a full replay: from_sequence {from_sequence} != 0 would restart the ID counter and mint wrong or duplicate trade IDs"
    )]
    NamespaceRequiresFullReplay {
        /// The non-zero starting sequence that was requested.
        from_sequence: u64,
    },

    /// A replayed mass cancel or eviction did not reproduce, by identity,
    /// the outcome the journal recorded for it (#252).
    ///
    /// Mass cancels are reconciled by **identity**, not by count: equal
    /// counts can hide different cancelled orders. For `CancelAll`,
    /// `CancelBySide`, `CancelByUser` and `CancelByPriceRange` journaled as
    /// a non-refused [`SequencerResult::MassCancelled`], replay re-executes
    /// the command and requires the replayed
    /// [`MassCancelResult::cancelled_order_ids`] to equal the journaled ones
    /// **in order**, the orders left resting by per-order failures
    /// ([`MassCancelResult::failed_order_ids`]) to equal the journaled ones
    /// in order, and the replay not to refuse. For an `EvictExpiredOrders`
    /// journaled as `MassCancelled`, replay evicts exactly the journaled ids
    /// and requires every one of them to be evicted, in order.
    ///
    /// [`MassCancelFailure::LevelFaultAfterRemoval`] entries are fault
    /// reports about the live price level, not book outcomes (the order was
    /// cancelled either way), and are not compared. A live mass cancel that
    /// recorded per-order failures ([`MassCancelFailure::OrderCancelFailed`])
    /// normally stops replay here **by design**: a fresh replay book does
    /// not reproduce the level fault, so it cancels the orders the live
    /// book kept, and the reconstructed book would diverge silently
    /// otherwise.
    #[error(
        "replay diverged at sequence {sequence_num}: mass cancel {divergence} (journal recorded {}, replay produced {})",
        .recorded,
        .replayed
    )]
    MassCancelMismatch {
        /// The sequence number of the mass cancel / eviction event.
        sequence_num: u64,
        /// What disagreed.
        divergence: MassCancelDivergence,
        /// The result the journal recorded.
        recorded: Box<MassCancelResult>,
        /// The result the replay produced.
        replayed: Box<MassCancelResult>,
    },

    /// The replayed state does not match the expected snapshot.
    #[error("snapshot mismatch: replayed state diverges from expected snapshot")]
    SnapshotMismatch,

    /// Journal read error during replay.
    #[error("journal error during replay: {0}")]
    JournalError(#[from] JournalError),
}

/// What a replayed mass cancel disagreed on with the journal; carried by
/// [`ReplayError::MassCancelMismatch`] (#252).
///
/// Positions index the ordered id lists; when one list is a prefix of the
/// other, `position` is the length of the shorter one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MassCancelDivergence {
    /// One side refused the whole call
    /// ([`MassCancelResult::is_refused`]) and the other did not.
    Refusal {
        /// Whether the journaled result was a refusal.
        recorded: bool,
        /// Whether the replayed result was a refusal.
        replayed: bool,
    },
    /// The cancelled order ids differ (identity or order).
    CancelledIds {
        /// First index at which the id lists differ.
        position: usize,
        /// Number of ids the journal recorded as cancelled.
        recorded_count: usize,
        /// Number of ids the replay cancelled.
        replayed_count: usize,
    },
    /// The orders left resting by per-order failures differ (identity or
    /// order).
    FailedIds {
        /// First index at which the failed-id lists differ.
        position: usize,
        /// Number of per-order failures the journal recorded.
        recorded_count: usize,
        /// Number of per-order failures the replay produced.
        replayed_count: usize,
    },
}

impl std::fmt::Display for MassCancelDivergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MassCancelDivergence::Refusal { recorded, replayed } => write!(
                f,
                "refusal differs (journal refused: {recorded}, replay refused: {replayed})"
            ),
            MassCancelDivergence::CancelledIds {
                position,
                recorded_count,
                replayed_count,
            } => write!(
                f,
                "cancelled ids differ at position {position} ({recorded_count} recorded, {replayed_count} replayed)"
            ),
            MassCancelDivergence::FailedIds {
                position,
                recorded_count,
                replayed_count,
            } => write!(
                f,
                "orders left resting differ at position {position} ({recorded_count} recorded, {replayed_count} replayed)"
            ),
        }
    }
}

/// First index at which two ordered id lists differ, or `None` when equal.
#[must_use]
fn first_difference<I: PartialEq>(recorded: &[I], replayed: &[I]) -> Option<usize> {
    match recorded.iter().zip(replayed).position(|(a, b)| a != b) {
        Some(position) => Some(position),
        None if recorded.len() == replayed.len() => None,
        None => Some(recorded.len().min(replayed.len())),
    }
}

/// Renders the `reached` field of a [`ReplayError::JournalTruncated`].
#[cold]
fn describe_reached(reached: Option<u64>) -> String {
    match reached {
        Some(seq) => format!("sequence {seq}"),
        None => "no entry".to_string(),
    }
}

/// Renders the replay side of an [`ReplayError::OutcomeMismatch`].
fn describe_outcome(actual: &Option<OrderBookError>) -> String {
    match actual {
        None => "succeeded".to_string(),
        Some(err) => format!("failed with `{}` ({err})", RejectReason::from(err)),
    }
}

/// Stateless replay engine that reconstructs [`OrderBook`] state from a [`Journal`].
///
/// All methods are associated functions (no `&self` receiver) — `ReplayEngine`
/// holds no state itself. Use it as a namespace for replay operations.
pub struct ReplayEngine<T> {
    _phantom: PhantomData<T>,
}

impl<T> ReplayEngine<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Default + 'static,
{
    /// Replays all events from `from_sequence` onwards onto a fresh [`OrderBook`].
    ///
    /// Returns the reconstructed book and the sequence number of the last
    /// event applied — the last command dispatched to the book, whether or
    /// not it changed the book.
    ///
    /// # Which events are applied
    ///
    /// Every successfully-journaled command is dispatched and must succeed
    /// again ([`ReplayError::OrderBookError`] otherwise). A rejected event
    /// is handled by what the journal recorded:
    ///
    /// - A submit (`AddOrder`, `MarketOrder`, `MarketOrderByAmount`)
    ///   journaled as [`SequencerResult::RejectedWithCode`] is re-executed
    ///   when replay can reproduce its rejection from the book state and
    ///   the book configuration alone: the two codes the engine returns
    ///   after it may already have traded —
    ///   [`RejectReason::InsufficientLiquidity`] (an IOC or market
    ///   remainder) and [`RejectReason::SelfTradePrevention`] (a taker
    ///   cancelled after non-self fills) — and the pure admission
    ///   rejections (tick, lot, size band, duplicate id, missing user,
    ///   post-only crossing). The re-execution reproduces the fills the
    ///   live command made before failing, and must fail under the same
    ///   [`RejectReason`]; a success, or a different code, aborts with
    ///   [`ReplayError::OutcomeMismatch`] because the reconstructed book
    ///   has diverged. A code whose trigger lives outside the
    ///   configuration — the kill switch, the risk limits,
    ///   [`RejectReason::Other`] — is skipped: those rejections never
    ///   touch the book, so the skip reproduces the live outcome exactly.
    /// - A submit the journal flagged as possibly post-mutation
    ///   (`may_have_mutated`) is re-executed **whatever its code says**.
    ///   That flag is what makes the residual-admission failure
    ///   replayable: the engine returns it as a `PriceLevelError` — which
    ///   maps to [`RejectReason::Other`]`(0)` — only after the sweep's
    ///   trades are irreversible, so the code-driven skip above would
    ///   rebuild the liquidity the live book consumed. Replay cannot
    ///   reproduce that failure (it is raised when a concurrent mutation
    ///   makes the residual unadmittable, not by anything in the journal),
    ///   so the re-execution normally rests the residual and the verdict
    ///   disagreement surfaces as [`ReplayError::OutcomeMismatch`] — a
    ///   loud stop instead of a silently wrong book.
    /// - An `AddOrder` journaled under
    ///   [`RejectReason::RiskRejectedAfterTrades`] (#291: the taker traded,
    ///   then the risk layer refused to reserve its residual) is
    ///   re-executed with its residual **refused** instead of rested. The
    ///   live refusal depended on the source book's `RiskConfig` and on
    ///   concurrent state, neither of which is in the journal or in
    ///   [`ReplayBookConfig`]; the sweep is a deterministic function of the
    ///   book and the order, so replay reproduces the live trades and ends
    ///   with the same book without needing the risk configuration. The
    ///   re-execution must fail under the same code: a sweep that fills the
    ///   whole order (nothing to refuse) or does not trade at all is
    ///   [`ReplayError::OutcomeMismatch`]. Journals written before this
    ///   code existed recorded such a failure under a pre-trade risk code
    ///   with `may_have_mutated: false`; replay cannot tell it from a
    ///   pre-trade rejection and skips it, so for those journals only
    ///   [`snapshots_match`] detects the missing trades.
    /// - A submit journaled as [`SequencerResult::MatchAborted`] (#240) is
    ///   re-executed and must abort again with the **same committed
    ///   prefix** — the same makers, prices and quantities in order, and
    ///   the same executed quantity. A replay that succeeds, fails
    ///   differently or commits different fills is
    ///   [`ReplayError::OutcomeMismatch`]. Aborts come from exhausted
    ///   resources — the trade-id generator's sequence (its counter is not
    ///   in the snapshot package or [`ReplayBookConfig`]), level counters,
    ///   an allocator refusal — that a fresh replay book usually does not
    ///   reproduce, so a journaled abort normally stops replay with
    ///   `OutcomeMismatch` **by design**: loud, never a silent divergence
    ///   in which the reconstructed book trades past the point where the
    ///   live one stopped. See `doc/panic-boundaries.md`. An `UpdateOrder` journaled as `RejectedWithCode` under
    ///   [`RejectReason::MatchAborted`] (its re-add aborted after the
    ///   original was cancelled), [`RejectReason::ModifyRolledBack`] (its
    ///   re-add failed after the cancel and the original was restored at
    ///   the back of its level) or [`RejectReason::ModifyOrderLost`] (the
    ///   order is gone, #247) is likewise re-executed and reconciled by
    ///   code. Those failures come from a concurrent mutation or a resource
    ///   the journal does not carry, so the re-execution normally succeeds
    ///   and replay stops with [`ReplayError::OutcomeMismatch`]: loud, never
    ///   a silent divergence.
    /// - A submit journaled as the string-only [`SequencerResult::Rejected`]
    ///   is skipped, as it always was: without a code replay cannot tell a
    ///   pure rejection from one that traded first, so such a journal keeps
    ///   the pre-existing gap for traded-then-rejected submits. Producers
    ///   close it by recording `RejectedWithCode`
    ///   (`SequencerResult::from(&OrderBookError)`).
    /// - Every other rejected command is skipped, including one flagged
    ///   `may_have_mutated`. That is sound for `CancelOrder` and for an
    ///   `UpdateOrder` not rejected under `MatchAborted`,
    ///   `ModifyRolledBack` or `ModifyOrderLost`: the modify paths
    ///   validate before touching the book, a cancel whose level refuses
    ///   the removal mutates nothing, and a cancel whose level committed
    ///   the removal before failing is journaled as `OrderCancelled`, not
    ///   as a rejection (#248). The flag is derived from the error alone,
    ///   which cannot tell the atomic modify `PriceLevelError` from the
    ///   submit one, so the command kind decides. Mass cancels and
    ///   `EvictExpiredOrders` are the exceptions with their own
    ///   reconciliation: they never fail after mutating (a partial outcome
    ///   is an `Ok` result carrying per-order failures, journaled as
    ///   `MassCancelled`), and their only `Err` is a refusal that changed
    ///   nothing.
    /// - A mass cancel journaled as a refused `MassCancelled` is skipped.
    ///   Any other journaled `MassCancelled` is reconciled by **identity**
    ///   (#252): a re-executed mass cancel must cancel the same ids in the
    ///   same order and leave the same orders resting, and a replayed
    ///   eviction evicts exactly the journaled ids. A disagreement is
    ///   [`ReplayError::MassCancelMismatch`].
    ///
    /// Skipped events do not advance the applied sequence or the applied
    /// count; a re-executed rejection does, whether or not it traded. Both
    /// the returned `last_applied_seq` and the progress callback's count
    /// therefore report what replay **dispatched**, which for a journal
    /// carrying re-executed rejections is more than the number of events
    /// that changed the book. Journals are expected to record the outcome
    /// the command API returned: a success recorded for a command that
    /// returned `Err` (a `TradeExecuted` captured from a `TradeListener`
    /// before `add_order` failed, say) is not a supported convention and
    /// aborts as a success/failure disagreement.
    ///
    /// `MarketOrder` / `MarketOrderByAmount` carry no user id, so they
    /// replay through the STP-less submit paths; a market order journaled
    /// after STP effects under a user cannot be represented, and a
    /// rejection recorded for it aborts with `OutcomeMismatch` rather than
    /// diverging silently.
    ///
    /// # What reconciliation does not cover
    ///
    /// Only the reject **code** is compared. The error's details — the
    /// `requested` / `available` quantities on
    /// [`RejectReason::InsufficientLiquidity`], the maker the STP scan
    /// hit — and the fills behind the rejection are not, so a discrepancy
    /// confined to them can go undetected: different fills can exhaust the
    /// same levels and leave identical books. The same holds for a
    /// [`ReplayBookConfig`] that differs in a way the code cannot see. The
    /// worked case is STP: a taker that fills a foreign maker and then
    /// reaches its own is refused under both `CancelTaker` and
    /// `CancelBoth`, and both report
    /// [`RejectReason::SelfTradePrevention`], but only `CancelBoth`
    /// also cancels that same-user maker.
    ///
    /// Two things narrow that gap, neither of which closes it:
    ///
    /// - Replay refuses a journaled STP rejection whose recorded
    ///   [`STPMode`] differs from the replay book's
    ///   ([`ReplayError::StpModeMismatch`]), which catches exactly the
    ///   case above — but only for journals that recorded at least one STP
    ///   rejection. A mode difference in a run that never prevented a
    ///   self-trade stays invisible.
    /// - [`snapshots_match`] is the oracle that does catch a diverged
    ///   book, by comparing the reconstructed state field by field against
    ///   a snapshot of the source book. `replay_from` performs no such
    ///   comparison; run it yourself, or use [`Self::verify`].
    ///
    /// Matching the source book's configuration remains the caller's
    /// contract on every `*_with_config` entry point.
    ///
    /// For deterministic replay with a custom clock, see
    /// [`Self::replay_from_with_clock`].
    ///
    /// # Configuration
    ///
    /// This entry point builds the target book with **all configuration at its
    /// defaults** (`tick_size` / `lot_size` / `min_order_size` /
    /// `max_order_size` = `None`, `stp_mode` = [`STPMode::None`],
    /// `fee_schedule` = `None`). It is therefore only valid for replaying a
    /// journal that was produced by a **default-config** book. A book that used
    /// tick / lot / STP / fees must be replayed through
    /// [`Self::replay_from_with_config`] (or
    /// [`Self::replay_from_with_clock_and_config`]) with the matching
    /// [`ReplayBookConfig`], or the reconstructed state will diverge from the
    /// original (and `snapshots_match` will report a mismatch). Likewise,
    /// the fresh book gets a random trade-ID namespace, so replayed trade
    /// IDs differ from the live ones unless a namespace-carrying config is
    /// used.
    ///
    /// # Arguments
    ///
    /// * `journal` — the event source
    /// * `from_sequence` — first sequence number to include (inclusive); pass `0` for full replay
    /// * `symbol` — symbol used to create the fresh OrderBook
    ///
    /// # Errors
    ///
    /// - [`ReplayError::EmptyJournal`] if the journal has no events
    /// - [`ReplayError::InvalidSequence`] if `from_sequence` > last journal sequence
    /// - [`ReplayError::OrderBookError`] if a command fails unexpectedly during replay
    /// - [`ReplayError::OutcomeMismatch`] if a re-executed rejected submit reaches a different verdict than the journal recorded
    /// - [`ReplayError::StpModeMismatch`] if a journaled STP rejection records a different [`STPMode`] than the replay book uses
    /// - [`ReplayError::MassCancelMismatch`] if a journaled mass cancel or eviction does not replay to the same ids
    /// - [`ReplayError::JournalError`] if reading from the journal fails, including its last sequence
    /// - [`ReplayError::JournalTruncated`] if the journal's entries end before the last sequence it reports
    #[must_use = "replay result carries the reconstructed book and the last applied sequence"]
    pub fn replay_from(
        journal: &impl Journal<T>,
        from_sequence: u64,
        symbol: &str,
    ) -> Result<(OrderBook<T>, u64), ReplayError> {
        Self::replay_from_with_progress(journal, from_sequence, symbol, |_, _| {})
    }

    /// Replays events with a progress callback invoked after each applied event.
    ///
    /// The callback receives `(events_applied: u64, current_sequence: u64)`.
    /// Useful for long replays where progress reporting is needed. "Applied"
    /// means dispatched to the book: skipped events do not fire the
    /// callback, and a re-executed rejection does — see the "Which events
    /// are applied" section on [`Self::replay_from`].
    ///
    /// For deterministic replay with a custom clock, see
    /// [`Self::replay_from_with_clock`].
    ///
    /// # Arguments
    ///
    /// * `journal` — the event source
    /// * `from_sequence` — first sequence number to include; pass `0` for full replay
    /// * `symbol` — symbol for the fresh OrderBook
    /// * `progress` — callback invoked after each event: `(events_applied, sequence_num)`
    ///
    /// # Errors
    ///
    /// Same as [`replay_from`](Self::replay_from).
    #[must_use = "replay result carries the reconstructed book and the last applied sequence"]
    pub fn replay_from_with_progress(
        journal: &impl Journal<T>,
        from_sequence: u64,
        symbol: &str,
        progress: impl Fn(u64, u64),
    ) -> Result<(OrderBook<T>, u64), ReplayError> {
        Self::check_replay_range(journal, from_sequence)?;

        let book = OrderBook::new(symbol);
        let last_applied_seq = Self::replay_into(&book, journal, from_sequence, progress)?;
        Ok((book, last_applied_seq))
    }

    /// Like [`Self::replay_from`] but injects a caller-supplied
    /// [`ReplayBookConfig`] into the fresh book **before** any events are
    /// replayed.
    ///
    /// This is the entry point for recovering a **non-default-config** book:
    /// the configuration (fees, STP, tick / lot / min / max order size) is
    /// applied to the target book so that a journal produced under that
    /// configuration reconstructs to the same structure and passes
    /// `snapshots_match` against the original. The configuration is supplied by
    /// the caller — it is not read from the journal, so the journal format is
    /// unchanged.
    ///
    /// For byte-identical timestamp reproduction (e.g. replay tests, or
    /// disaster-recovery that must match engine-assigned timestamps), use
    /// [`Self::replay_from_with_clock_and_config`].
    ///
    /// # Arguments
    ///
    /// * `journal` — the event source
    /// * `from_sequence` — first sequence number to include (inclusive); pass `0` for full replay
    /// * `symbol` — symbol used to create the fresh OrderBook
    /// * `config` — configuration the source book used, applied before replay
    ///
    /// # Errors
    ///
    /// Same as [`replay_from`](Self::replay_from), plus
    /// [`ReplayError::NamespaceRequiresFullReplay`] when `config` carries a
    /// `trade_id_namespace` and `from_sequence != 0`.
    #[must_use = "replay result carries the reconstructed book and the last applied sequence"]
    pub fn replay_from_with_config(
        journal: &impl Journal<T>,
        from_sequence: u64,
        symbol: &str,
        config: &ReplayBookConfig,
    ) -> Result<(OrderBook<T>, u64), ReplayError> {
        Self::check_replay_range(journal, from_sequence)?;

        Self::check_namespace_full_replay(from_sequence, config)?;

        let mut book = OrderBook::new(symbol);
        config.apply_to(&mut book);
        let last_applied_seq = Self::replay_into(&book, journal, from_sequence, |_, _| {})?;
        Ok((book, last_applied_seq))
    }

    /// Like [`Self::replay_from`] but injects a caller-supplied [`Clock`] into
    /// the reconstructed book.
    ///
    /// This is the canonical entry point for byte-identical replay tests and
    /// disaster-recovery pipelines that must reproduce engine-assigned
    /// timestamps deterministically. Pass a
    /// [`crate::orderbook::clock::StubClock`] for test and proptest-driven
    /// replay, or a [`crate::orderbook::clock::MonotonicClock`] for
    /// production disaster-recovery where wall-clock timestamps are
    /// acceptable.
    ///
    /// # Configuration
    ///
    /// Like [`Self::replay_from`], this builds the target book with **all
    /// configuration at its defaults** and is only valid for a default-config
    /// source book. To recover a book that used tick / lot / STP / fees
    /// deterministically, use [`Self::replay_from_with_clock_and_config`] with
    /// the matching [`ReplayBookConfig`]. The fresh book also gets a random
    /// trade-ID namespace: the injected clock makes timestamps byte-identical,
    /// but replayed trade IDs differ from the live ones unless the config
    /// path carries the live namespace.
    ///
    /// # Arguments
    ///
    /// * `journal` — the event source
    /// * `from_sequence` — first sequence number to include (inclusive); pass `0` for full replay
    /// * `symbol` — symbol used to create the fresh OrderBook
    /// * `clock` — pre-constructed clock shared across the reconstructed book
    ///
    /// # Errors
    ///
    /// Same as [`replay_from`](Self::replay_from).
    #[must_use = "replay result carries the reconstructed book and the last applied sequence"]
    pub fn replay_from_with_clock(
        journal: &impl Journal<T>,
        from_sequence: u64,
        symbol: &str,
        clock: Arc<dyn Clock>,
    ) -> Result<(OrderBook<T>, u64), ReplayError> {
        Self::replay_from_with_clock_and_progress(journal, from_sequence, symbol, clock, |_, _| {})
    }

    /// Like [`Self::replay_from_with_progress`] plus clock injection.
    ///
    /// Equivalent to [`Self::replay_from_with_clock`] but forwards each
    /// event it dispatched to the book — a re-executed rejection included,
    /// a skipped event not — to a progress callback. Useful for long
    /// replays where progress reporting is needed and byte-identical
    /// timestamp reproduction is required — the canonical entry point for
    /// byte-identical replay tests and disaster-recovery pipelines that must
    /// reproduce engine-assigned timestamps deterministically.
    ///
    /// # Arguments
    ///
    /// * `journal` — the event source
    /// * `from_sequence` — first sequence number to include; pass `0` for full replay
    /// * `symbol` — symbol for the fresh OrderBook
    /// * `clock` — pre-constructed clock shared across the reconstructed book
    /// * `progress` — callback invoked after each event: `(events_applied, sequence_num)`
    ///
    /// # Errors
    ///
    /// Same as [`replay_from`](Self::replay_from).
    #[must_use = "replay result carries the reconstructed book and the last applied sequence"]
    pub fn replay_from_with_clock_and_progress(
        journal: &impl Journal<T>,
        from_sequence: u64,
        symbol: &str,
        clock: Arc<dyn Clock>,
        progress: impl Fn(u64, u64),
    ) -> Result<(OrderBook<T>, u64), ReplayError> {
        Self::check_replay_range(journal, from_sequence)?;

        let book = OrderBook::with_clock(symbol, clock);
        let last_applied_seq = Self::replay_into(&book, journal, from_sequence, progress)?;
        Ok((book, last_applied_seq))
    }

    /// Like [`Self::replay_from_with_clock`] but also injects a caller-supplied
    /// [`ReplayBookConfig`] into the fresh book **before** any events are
    /// replayed.
    ///
    /// This is the canonical entry point for byte-identical, deterministic
    /// recovery of a **non-default-config** book: the injected [`Clock`]
    /// reproduces engine-assigned timestamps and the [`ReplayBookConfig`]
    /// reproduces the structural configuration (fees, STP, tick / lot / min /
    /// max order size), so the reconstructed book passes `snapshots_match`
    /// against the original. When the config also carries the live book's
    /// `trade_id_namespace` (#199), a **full** replay (`from_sequence == 0`)
    /// reproduces the live trade-ID stream byte-identically as well; suffix
    /// replays with a namespace are rejected
    /// ([`ReplayError::NamespaceRequiresFullReplay`]). The configuration is
    /// supplied by the caller — it is not read from the journal, so the
    /// journal format is unchanged.
    ///
    /// # Arguments
    ///
    /// * `journal` — the event source
    /// * `from_sequence` — first sequence number to include (inclusive); pass `0` for full replay
    /// * `symbol` — symbol used to create the fresh OrderBook
    /// * `clock` — pre-constructed clock shared across the reconstructed book
    /// * `config` — configuration the source book used, applied before replay
    ///
    /// # Errors
    ///
    /// Same as [`replay_from`](Self::replay_from), plus
    /// [`ReplayError::NamespaceRequiresFullReplay`] when `config` carries a
    /// `trade_id_namespace` and `from_sequence != 0`.
    #[must_use = "replay result carries the reconstructed book and the last applied sequence"]
    pub fn replay_from_with_clock_and_config(
        journal: &impl Journal<T>,
        from_sequence: u64,
        symbol: &str,
        clock: Arc<dyn Clock>,
        config: &ReplayBookConfig,
    ) -> Result<(OrderBook<T>, u64), ReplayError> {
        Self::check_replay_range(journal, from_sequence)?;

        Self::check_namespace_full_replay(from_sequence, config)?;

        // With an injected namespace the book is built without deriving the
        // default one, so a deterministic replay reads no wall clock, pid or
        // construction counter (see `OrderBook::with_clock_and_namespace`).
        let mut book = match config.trade_id_namespace {
            Some(namespace) => OrderBook::with_clock_and_namespace(symbol, clock, namespace),
            None => OrderBook::with_clock(symbol, clock),
        };
        config.apply_to(&mut book);
        let last_applied_seq = Self::replay_into(&book, journal, from_sequence, |_, _| {})?;
        Ok((book, last_applied_seq))
    }

    /// The `EmptyJournal` / `InvalidSequence` pre-checks shared by every
    /// public entry point.
    ///
    /// A journal whose last sequence cannot be read (for example a poisoned
    /// lock) is [`ReplayError::JournalError`], never reported as empty
    /// (#252).
    fn check_replay_range(
        journal: &impl Journal<T>,
        from_sequence: u64,
    ) -> Result<(), ReplayError> {
        let Some(last_seq) = journal.last_sequence()? else {
            return Err(ReplayError::EmptyJournal);
        };
        if from_sequence > last_seq {
            return Err(ReplayError::InvalidSequence {
                from_sequence,
                last_sequence: last_seq,
            });
        }
        Ok(())
    }

    /// Rejects a namespace-carrying config on a suffix replay.
    ///
    /// Applying a namespace restarts the trade-ID counter at 0, so the
    /// byte-identical guarantee only holds when replay starts at the origin
    /// of the trade-ID stream. `from_sequence == 0` additionally forces the
    /// journal itself to start at sequence 0 (gap detection rejects a
    /// journal whose first event is later), so the shipped API cannot
    /// silently mint wrong or duplicate IDs. See
    /// [`ReplayError::NamespaceRequiresFullReplay`].
    #[inline]
    fn check_namespace_full_replay(
        from_sequence: u64,
        config: &ReplayBookConfig,
    ) -> Result<(), ReplayError> {
        if from_sequence != 0 && config.trade_id_namespace.is_some() {
            return Err(ReplayError::NamespaceRequiresFullReplay { from_sequence });
        }
        Ok(())
    }

    /// Shared replay loop. Applies events from `journal` starting at
    /// `from_sequence` to the already-constructed `book`, reporting
    /// per-event progress via `progress`, and returns the last applied
    /// sequence number.
    ///
    /// Does not construct the book and does not perform the
    /// `EmptyJournal` / `InvalidSequence` pre-checks — those remain the
    /// responsibility of the public entry points so that the distinction
    /// between "the journal is empty" and "the journal exists but
    /// contains no matching range" is preserved.
    fn replay_into(
        book: &OrderBook<T>,
        journal: &impl Journal<T>,
        from_sequence: u64,
        progress: impl Fn(u64, u64),
    ) -> Result<u64, ReplayError> {
        let mut last_applied_seq = 0u64;
        let mut count = 0u64;
        let mut expected_seq = from_sequence;

        // Read before iterating: every entry up to this sequence is
        // committed, so the iteration below must reach it (#295).
        let target = journal.last_sequence()?;
        let iter = journal.read_from(from_sequence)?;

        for entry_result in iter {
            let entry = entry_result?;
            let event = &entry.event;

            // Gap detection
            if event.sequence_num != expected_seq {
                return Err(ReplayError::SequenceGap {
                    expected: expected_seq,
                    found: event.sequence_num,
                });
            }

            // Advance `expected_seq` before applying so gap detection stays
            // correct even if the event is skipped. `last_applied_seq`,
            // `count`, and `progress` track the events `apply_event`
            // dispatched to the book — including a re-executed rejection,
            // which may have traded before it failed — consistent with the
            // "events applied" / "last applied sequence" contract on the
            // public entry points. Applied means dispatched, not that the
            // book changed.
            let applied = Self::apply_event(book, event)?;
            // Protocol counter: a saturating add would silently stop advancing
            // `expected_seq` at the u64 ceiling and mask a real gap, so use a
            // checked add and surface a typed overflow error instead (per the
            // no-saturating-on-protocol-counters rule).
            expected_seq = expected_seq
                .checked_add(1)
                .ok_or(ReplayError::SequenceOverflow { at: expected_seq })?;

            if applied {
                last_applied_seq = event.sequence_num;
                count = count
                    .checked_add(1)
                    .ok_or(ReplayError::SequenceOverflow { at: count })?;
                progress(count, last_applied_seq);
            }
        }

        Self::ensure_reached(target, from_sequence, expected_seq)?;
        Ok(last_applied_seq)
    }

    /// Fails with [`ReplayError::JournalTruncated`] when the iteration that
    /// started at `from_sequence` and stopped before `next_seq` did not reach
    /// `target`, the journal's last sequence read before iterating.
    fn ensure_reached(
        target: Option<u64>,
        from_sequence: u64,
        next_seq: u64,
    ) -> Result<(), ReplayError> {
        let Some(expected_last) = target.filter(|last| *last >= from_sequence) else {
            return Ok(());
        };
        // Gap detection makes the read sequences contiguous from
        // `from_sequence`, so the last one read is `next_seq - 1`.
        let reached = if next_seq > from_sequence {
            next_seq.checked_sub(1)
        } else {
            None
        };
        if reached.is_some_and(|last| last >= expected_last) {
            return Ok(());
        }
        tracing::error!(
            expected_last,
            reached = ?reached,
            "journal iteration ended before its last sequence; replay refused"
        );
        Err(ReplayError::JournalTruncated {
            expected_last,
            reached,
        })
    }

    /// Replays the full journal and compares the result to an expected snapshot.
    ///
    /// Returns `Ok(true)` if the replayed state matches, `Ok(false)` if it
    /// diverges. The comparison uses [`snapshots_match`] which checks symbol,
    /// bid price levels, and ask price levels.
    ///
    /// This is the check that catches a divergence the per-event
    /// reconciliation cannot see — differing fills behind a rejection that
    /// carried the same reject code, or a [`ReplayBookConfig`] difference
    /// the code does not express. It replays with **default configuration**
    /// (see [`Self::replay_from`]), so a non-default-config book must be
    /// replayed through a `*_with_config` entry point and compared with
    /// [`snapshots_match`] directly.
    ///
    /// # Errors
    ///
    /// - [`ReplayError::EmptyJournal`] if the journal has no events
    /// - [`ReplayError::OrderBookError`] if replay fails, or if the replayed
    ///   book cannot produce a snapshot (reported at the last applied
    ///   sequence)
    /// - [`ReplayError::OutcomeMismatch`] if a re-executed rejected submit reaches a different verdict than the journal recorded
    /// - [`ReplayError::StpModeMismatch`] if a journaled STP rejection records a different [`STPMode`] than the replay book uses
    /// - [`ReplayError::MassCancelMismatch`] if a journaled mass cancel or eviction does not replay to the same ids
    /// - [`ReplayError::JournalError`] if reading from the journal fails
    pub fn verify(
        journal: &impl Journal<T>,
        expected_snapshot: &OrderBookSnapshot,
    ) -> Result<bool, ReplayError> {
        let (book, last_sequence) = Self::replay_from(journal, 0, &expected_snapshot.symbol)?;
        // `create_snapshot` is fallible since pricelevel 0.10; a level that
        // cannot be snapshotted is attributed to the last applied event.
        let actual =
            book.create_snapshot(usize::MAX)
                .map_err(|source| ReplayError::OrderBookError {
                    sequence_num: last_sequence,
                    source,
                })?;
        Ok(snapshots_match(&actual, expected_snapshot))
    }

    /// Applies a single sequencer event to the given book.
    ///
    /// Returns `true` when the command was dispatched to the book and its
    /// outcome reconciled against the journaled result, `false` when the
    /// event was skipped. Applied means dispatched, not that the book
    /// changed: a re-executed rejection that fails the same way it failed
    /// live is applied and leaves the book untouched.
    ///
    /// A successfully-journaled command is always dispatched. A rejected
    /// event is dispatched only when it is a submit — `AddOrder`,
    /// `MarketOrder`, `MarketOrderByAmount` — journaled as
    /// [`SequencerResult::RejectedWithCode`] that
    /// [`Self::replays_rejection`] accepts. That is what makes the
    /// rejections that follow a mutation faithful: an IOC whose remainder
    /// is unfillable and a taker STP cancels after non-self fills both
    /// execute real trades and *then* return `Err`, so skipping them
    /// resurrected liquidity the live book had consumed. Replay
    /// re-executes matching deterministically, so those fills are
    /// reproduced by the re-execution itself; the recorded code is read
    /// only to check the verdict, in [`Self::reconcile_submit`]. An
    /// `AddOrder` recorded under [`RejectReason::RiskRejectedAfterTrades`]
    /// is re-executed with its residual refused (#291), since the live
    /// refusal came from a `RiskConfig` replay does not have.
    ///
    /// Every other rejected event is skipped: a string-only
    /// [`SequencerResult::Rejected`] carries no code to decide by (the
    /// historical behaviour, and the documented gap for such journals),
    /// and a rejected `CancelOrder` or non-`MatchAborted` `UpdateOrder` is
    /// failure-atomic — the modify paths validate before touching the book,
    /// a cancel whose level refuses the removal mutates nothing, and a
    /// cancel whose level committed the removal is journaled as
    /// `OrderCancelled`, not as a rejection (#248). Mass cancels and
    /// eviction are the exceptions with their own reconciliation: they
    /// never fail after mutating (a partial outcome is an `Ok` result with
    /// per-order failures, journaled as `MassCancelled`), and the only `Err`
    /// an eviction returns is a refusal that evicted nothing, so skipping
    /// its journaled rejection is faithful.
    ///
    /// Before any of that, a journaled rejection that carries the source
    /// book's [`STPMode`] is checked against the replay book's
    /// ([`Self::check_stp_mode`]) — a skipped event included, since the
    /// mismatch is a configuration error either way.
    ///
    /// The full contract, including the stated limitations, is documented
    /// on [`Self::replay_from`].
    fn apply_event(book: &OrderBook<T>, event: &SequencerEvent<T>) -> Result<bool, ReplayError> {
        let is_submit = matches!(
            event.command,
            SequencerCommand::AddOrder(_)
                | SequencerCommand::MarketOrder { .. }
                | SequencerCommand::MarketOrderByAmount { .. }
        );
        // #240: an update whose re-add aborted mid-sweep after the original
        // was cancelled changed the book although it failed, so it is
        // re-executed like a submit and reconciled by code. #247: so is one
        // whose re-add failed after the cancel and was rolled back (the
        // original moved to the back of its level) or lost the order.
        let is_update = matches!(event.command, SequencerCommand::UpdateOrder(_));
        // The reject code the journal recorded for a submit replay
        // re-executes; `None` for a journaled success.
        let recorded = match &event.result {
            SequencerResult::Rejected { .. } => return Ok(false),
            SequencerResult::RejectedWithCode {
                code,
                may_have_mutated,
                stp_mode,
                ..
            } => {
                Self::check_stp_mode(book, event, *stp_mode)?;
                let replays_update = is_update
                    && matches!(
                        code,
                        RejectReason::MatchAborted
                            | RejectReason::ModifyRolledBack
                            | RejectReason::ModifyOrderLost
                    );
                if !(is_submit || replays_update)
                    || !Self::replays_rejection(*code, *may_have_mutated)
                {
                    return Ok(false);
                }
                Some(*code)
            }
            SequencerResult::MatchAborted {
                code, committed, ..
            } => {
                if is_submit {
                    Self::replay_aborted_submit(book, event, committed)?;
                    return Ok(true);
                }
                if !is_update {
                    return Ok(false);
                }
                Some(*code)
            }
            _ => None,
        };

        match &event.command {
            // #291: the live taker traded and the risk layer then refused
            // its residual. The refusal depended on the source book's
            // `RiskConfig` and concurrent state, which replay does not
            // have; the sweep did not. Replay reproduces the sweep and
            // refuses the residual as recorded, then reconciles the code.
            SequencerCommand::AddOrder(order)
                if recorded == Some(RejectReason::RiskRejectedAfterTrades) =>
            {
                Self::reconcile_submit(
                    event,
                    recorded,
                    book.replay_add_order_refusing_residual(order.clone())
                        .map(|_| ()),
                )?;
            }
            SequencerCommand::AddOrder(order) => {
                Self::reconcile_submit(event, recorded, book.add_order(order.clone()).map(|_| ()))?;
            }
            SequencerCommand::CancelOrder(id) => {
                book.cancel_order(*id)
                    .map_err(|e| ReplayError::OrderBookError {
                        sequence_num: event.sequence_num,
                        source: e,
                    })?;
            }
            // #248: an update journaled as `OrderCancelled` removed the
            // order and did nothing else: a zero-quantity update, or a
            // cancel-then-add modify whose cancel the level committed and
            // then failed (`OrderRemovedWithLevelFault`, recorded as the
            // cancel it was). Re-executing the update would re-add the
            // order, so replay applies the recorded cancel instead.
            SequencerCommand::UpdateOrder(_)
                if matches!(event.result, SequencerResult::OrderCancelled { .. }) =>
            {
                if let SequencerResult::OrderCancelled { order_id } = &event.result {
                    book.cancel_order(*order_id)
                        .map_err(|e| ReplayError::OrderBookError {
                            sequence_num: event.sequence_num,
                            source: e,
                        })?;
                }
            }
            SequencerCommand::UpdateOrder(update) => {
                // Only a journaled `MatchAborted`, `ModifyRolledBack` or
                // `ModifyOrderLost` rejection reaches here with `recorded`
                // set (see above); a journaled success is reconciled exactly
                // as before.
                Self::reconcile_submit(event, recorded, book.update_order(*update).map(|_| ()))?;
            }
            SequencerCommand::MarketOrder { id, quantity, side } => {
                Self::reconcile_submit(
                    event,
                    recorded,
                    book.submit_market_order(*id, *quantity, *side).map(|_| ()),
                )?;
            }
            SequencerCommand::MarketOrderByAmount { id, amount, side } => {
                Self::reconcile_submit(
                    event,
                    recorded,
                    book.submit_market_order_by_amount(*id, *amount, *side)
                        .map(|_| ()),
                )?;
            }
            // A mass cancel the live book refused cancelled nothing, so
            // replay applies it as a no-op instead of re-executing it: the
            // replay book may be readable where the live one was not, and
            // re-execution would then cancel orders the live book kept.
            SequencerCommand::CancelAll
            | SequencerCommand::CancelBySide { .. }
            | SequencerCommand::CancelByUser { .. }
            | SequencerCommand::CancelByPriceRange { .. }
                if Self::recorded_mass_cancel_refused(event) => {}
            SequencerCommand::CancelAll => {
                Self::reconcile_mass_cancel(event, book.cancel_all_orders())?;
            }
            SequencerCommand::CancelBySide { side } => {
                Self::reconcile_mass_cancel(event, book.cancel_orders_by_side(*side))?;
            }
            SequencerCommand::CancelByUser { user_id } => {
                Self::reconcile_mass_cancel(event, book.cancel_orders_by_user(*user_id))?;
            }
            SequencerCommand::CancelByPriceRange {
                side,
                min_price,
                max_price,
            } => {
                Self::reconcile_mass_cancel(
                    event,
                    book.cancel_orders_by_price_range(*side, *min_price, *max_price),
                )?;
            }
            SequencerCommand::EvictExpiredOrders { now_ms } => match &event.result {
                // A sweep journaled as refused evicted nothing.
                SequencerResult::MassCancelled { result } if result.is_refused() => {}
                // #248: the journal names the evicted orders. Evict exactly
                // those, never re-run the sweep: an order the live sweep
                // failed to evict must keep resting on replay too.
                SequencerResult::MassCancelled { result } => {
                    let replayed = book.evict_orders_by_id(result.cancelled_order_ids());
                    Self::ensure_same_evictions(event, result, replayed)?;
                }
                // No journaled outcome to follow: apply the journaled cutoff,
                // never the replay clock. A refused or partial sweep diverges
                // from a live one that completed, so surface it.
                _ => {
                    let replayed = book.evict_expired_orders(*now_ms).map_err(|source| {
                        ReplayError::OrderBookError {
                            sequence_num: event.sequence_num,
                            source,
                        }
                    })?;
                    Self::ensure_replay_complete(event, replayed.mass_cancel_result())?;
                }
            },
        }

        Ok(true)
    }

    /// Whether the journal recorded this mass cancel as refused.
    ///
    /// A refusal ([`MassCancelResult::is_refused`]) cancelled nothing on
    /// the live book, so replay must not re-execute it. A result carrying
    /// only per-order failures (#248) is partial, not refused: its listed
    /// ids were cancelled live, so replay re-executes it and reconciles it
    /// by identity (see [`Self::reconcile_mass_cancel`]).
    fn recorded_mass_cancel_refused(event: &SequencerEvent<T>) -> bool {
        matches!(
            &event.result,
            SequencerResult::MassCancelled { result } if result.is_refused()
        )
    }

    /// Reconciles a re-executed scoped mass cancel against the journal by
    /// identity (#252; see [`ReplayError::MassCancelMismatch`]).
    ///
    /// The journaled [`SequencerResult::MassCancelled`] result is the
    /// reference: the replayed cancelled ids and the replayed per-order
    /// failure ids must equal the journaled ones in order, and the replay
    /// must not refuse (a journaled refusal never gets here, see
    /// [`Self::recorded_mass_cancel_refused`]). An event journaled with any
    /// other success result carries no ids to compare against, so replay
    /// only requires the re-execution to complete
    /// ([`Self::ensure_replay_complete`]).
    fn reconcile_mass_cancel(
        event: &SequencerEvent<T>,
        replayed: MassCancelResult,
    ) -> Result<(), ReplayError> {
        let SequencerResult::MassCancelled { result: recorded } = &event.result else {
            return Self::ensure_replay_complete(event, &replayed);
        };
        let recorded_failed: Vec<_> = recorded.failed_order_ids().collect();
        let replayed_failed: Vec<_> = replayed.failed_order_ids().collect();
        let divergence = if recorded.is_refused() != replayed.is_refused() {
            Some(MassCancelDivergence::Refusal {
                recorded: recorded.is_refused(),
                replayed: replayed.is_refused(),
            })
        } else if let Some(position) = first_difference(
            recorded.cancelled_order_ids(),
            replayed.cancelled_order_ids(),
        ) {
            Some(MassCancelDivergence::CancelledIds {
                position,
                recorded_count: recorded.cancelled_order_ids().len(),
                replayed_count: replayed.cancelled_order_ids().len(),
            })
        } else {
            first_difference(&recorded_failed, &replayed_failed).map(|position| {
                MassCancelDivergence::FailedIds {
                    position,
                    recorded_count: recorded_failed.len(),
                    replayed_count: replayed_failed.len(),
                }
            })
        };
        match divergence {
            None => Ok(()),
            Some(divergence) => Err(Self::mass_cancel_mismatch(
                event, divergence, recorded, replayed,
            )),
        }
    }

    /// Fails replay when a re-executed mass cancel or sweep with no
    /// journaled result to compare against recorded a failure other than a
    /// level fault report.
    ///
    /// A refused call cancels nothing and a partial one leaves orders
    /// resting (see [`MassCancelFailure`]), so continuing would leave the
    /// replayed book diverged from the live one without any signal. The
    /// first such failure is reported as [`ReplayError::OrderBookError`] at
    /// the event's sequence number.
    fn ensure_replay_complete(
        event: &SequencerEvent<T>,
        result: &MassCancelResult,
    ) -> Result<(), ReplayError> {
        match result
            .failures()
            .iter()
            .find(|failure| !matches!(failure, MassCancelFailure::LevelFaultAfterRemoval { .. }))
        {
            None => Ok(()),
            Some(failure) => Err(ReplayError::OrderBookError {
                sequence_num: event.sequence_num,
                source: failure.to_order_book_error(),
            }),
        }
    }

    /// Fails replay unless a journaled eviction removed exactly the
    /// journaled ids, in order, on the replay book (#248, #252).
    ///
    /// `replayed` comes from evicting the recorded ids by identity, so any
    /// disagreement means the replay book no longer holds an order the live
    /// sweep evicted (or its level refused it). The orders the live sweep
    /// failed to evict are not attempted, so they keep resting and there is
    /// no failure list to compare. Reported as
    /// [`ReplayError::MassCancelMismatch`].
    fn ensure_same_evictions(
        event: &SequencerEvent<T>,
        recorded: &MassCancelResult,
        replayed: MassCancelResult,
    ) -> Result<(), ReplayError> {
        match first_difference(
            recorded.cancelled_order_ids(),
            replayed.cancelled_order_ids(),
        ) {
            None => Ok(()),
            Some(position) => Err(Self::mass_cancel_mismatch(
                event,
                MassCancelDivergence::CancelledIds {
                    position,
                    recorded_count: recorded.cancelled_order_ids().len(),
                    replayed_count: replayed.cancelled_order_ids().len(),
                },
                recorded,
                replayed,
            )),
        }
    }

    /// Builds (and logs) a [`ReplayError::MassCancelMismatch`].
    #[cold]
    #[inline(never)]
    fn mass_cancel_mismatch(
        event: &SequencerEvent<T>,
        divergence: MassCancelDivergence,
        recorded: &MassCancelResult,
        replayed: MassCancelResult,
    ) -> ReplayError {
        tracing::error!(
            sequence_num = event.sequence_num,
            %divergence,
            "replay diverged: mass cancel outcome differs from the journal"
        );
        ReplayError::MassCancelMismatch {
            sequence_num: event.sequence_num,
            divergence,
            recorded: Box::new(recorded.clone()),
            replayed: Box::new(replayed),
        }
    }

    /// Whether a submit journaled as rejected under `code` is re-executed
    /// on replay.
    ///
    /// `may_have_mutated` is the journal's record of whether the engine
    /// could already have changed the book when it returned the error (see
    /// [`SequencerResult::RejectedWithCode`]). It **overrides the table
    /// below**: a rejection that may have mutated must be re-executed, or
    /// replay silently loses the mutation, and the re-executed verdict is
    /// checked either way. That is what makes the residual-admission
    /// failure replayable — the engine returns it as a `PriceLevelError`
    /// after the sweep's trades are irreversible, and `PriceLevelError`
    /// maps to `Other(0)`, which the table skips.
    ///
    /// Re-executed by code: every code whose rejection the engine derives
    /// from the book state and [`ReplayBookConfig`] alone, so the
    /// re-execution reproduces the live verdict. That covers the two codes
    /// the engine returns after it may already have traded —
    /// [`RejectReason::InsufficientLiquidity`] (an IOC or market remainder)
    /// and [`RejectReason::SelfTradePrevention`] (a taker cancelled after
    /// non-self fills) — and the pure admission rejections (tick, lot, size
    /// band, duplicate id, missing user, post-only crossing), which
    /// re-derive the same no-op and double as a check that the config
    /// matches the source book.
    ///
    /// Skipped by code: codes whose trigger lives outside the config — the
    /// kill switch ([`RejectReason::KillSwitchActive`]), the per-account
    /// risk limits (`RiskMaxOpenOrders` / `RiskMaxNotional` /
    /// `RiskPriceBand`; a `RiskConfig` is not part of `ReplayBookConfig`)
    /// and application-side or internal codes ([`RejectReason::Other`]).
    /// The risk codes are pre-trade only: a risk refusal that follows
    /// trades is [`RejectReason::RiskRejectedAfterTrades`] (#291), which is
    /// not in this table and is re-executed with its residual refused (see
    /// [`Self::replay_from`]).
    /// None of them mutates the book, so skipping reproduces the live
    /// outcome exactly, while re-executing would apply a command the live
    /// book refused: under an engaged kill switch a rejected GTC would rest
    /// on replay, and a rejected IOC would consume liquidity the live book
    /// never touched. These are **skipped, not re-executed** — replay does
    /// not turn a missing kill switch or `RiskConfig` into an
    /// [`ReplayError::OutcomeMismatch`], because a rejection that never
    /// touched the book is reproduced exactly by doing nothing.
    ///
    /// `Other(0)` is the library's own bucket for errors that are not
    /// public rejects. For a submit that is the clock-dependent
    /// expired-at-admission `InvalidOperation`, which a replay clock cannot
    /// be expected to reproduce and which is skipped like the rest of the
    /// table, and the residual-rest `PriceLevelError`, which the
    /// `may_have_mutated` flag pulls back into re-execution.
    ///
    /// `RejectReason` is `#[non_exhaustive]`; a named code this table does
    /// not list is re-executed, so a divergence surfaces as
    /// [`ReplayError::OutcomeMismatch`] rather than being skipped silently.
    #[must_use]
    #[inline]
    fn replays_rejection(code: RejectReason, may_have_mutated: bool) -> bool {
        may_have_mutated
            || !matches!(
                code,
                RejectReason::KillSwitchActive
                    | RejectReason::RiskMaxOpenOrders
                    | RejectReason::RiskMaxNotional
                    | RejectReason::RiskPriceBand
                    | RejectReason::Other(_)
            )
    }

    /// Rejects a journal whose recorded self-trade-prevention mode differs
    /// from the replay book's.
    ///
    /// `recorded` is `Some` only for a rejection the STP scan produced,
    /// because only `OrderBookError::SelfTradePrevented` carries the mode
    /// that decided it. When it disagrees with the replay book's
    /// [`OrderBook::stp_mode`], the [`ReplayBookConfig`] does not describe
    /// the source book and replay stops with
    /// [`ReplayError::StpModeMismatch`] instead of reconstructing a book
    /// that differs in ways the reject code cannot express: `CancelTaker`
    /// and `CancelBoth` refuse the same taker under the same code while
    /// only the second cancels the same-user maker.
    ///
    /// Coverage is a lower bound. The journal only learns the source mode
    /// from a rejection that fired, so a mode difference in a run that
    /// never prevented a self-trade passes this check; and a run that
    /// changed its mode mid-stream cannot be replayed under a single
    /// config, which this check reports as a mismatch at the first event
    /// recorded under the other mode.
    fn check_stp_mode(
        book: &OrderBook<T>,
        event: &SequencerEvent<T>,
        recorded: Option<STPMode>,
    ) -> Result<(), ReplayError> {
        let Some(recorded) = recorded else {
            return Ok(());
        };
        let actual = book.stp_mode();
        if recorded == actual {
            return Ok(());
        }
        Err(ReplayError::StpModeMismatch {
            sequence_num: event.sequence_num,
            recorded,
            actual,
        })
    }

    /// Re-executes a submit journaled as [`SequencerResult::MatchAborted`]
    /// and checks that it aborts again with the recorded committed prefix
    /// (#240).
    ///
    /// Dispatched through the `*_with_committed` submit entry points, the
    /// only ones that return the committed trades together with the error.
    /// A market command carries no user identity, so it replays through the
    /// STP-less path, as the other market replays do.
    ///
    /// # Errors
    ///
    /// [`ReplayError::OutcomeMismatch`] when the re-execution succeeds,
    /// fails under another code, or aborts with different fills;
    /// [`ReplayError::OrderBookError`] when the replayed prefix cannot be
    /// recorded for the comparison.
    #[cold]
    #[inline(never)]
    fn replay_aborted_submit(
        book: &OrderBook<T>,
        event: &SequencerEvent<T>,
        recorded: &CommittedPrefix,
    ) -> Result<(), ReplayError> {
        let outcome: Result<(), SubmitFailure> = match &event.command {
            SequencerCommand::AddOrder(order) => {
                book.add_order_with_committed(order.clone()).map(|_| ())
            }
            SequencerCommand::MarketOrder { id, quantity, side } => book
                .submit_market_order_with_committed(*id, *quantity, *side)
                .map(|_| ()),
            SequencerCommand::MarketOrderByAmount { id, amount, side } => book
                .submit_market_order_by_amount_with_committed(*id, *amount, *side)
                .map(|_| ()),
            // `apply_event` only routes submits here.
            _ => return Ok(()),
        };
        let failure = match outcome {
            Ok(()) => {
                return Err(ReplayError::OutcomeMismatch {
                    sequence_num: event.sequence_num,
                    recorded: RejectReason::MatchAborted,
                    actual: None,
                });
            }
            Err(failure) => failure,
        };
        if !matches!(failure.error, OrderBookError::MatchAborted { .. }) {
            return Err(ReplayError::OutcomeMismatch {
                sequence_num: event.sequence_num,
                recorded: RejectReason::MatchAborted,
                actual: Some(failure.error),
            });
        }
        let replayed = match &failure.committed {
            Some(trade_result) => CommittedPrefix::try_from_match_result(
                &trade_result.match_result,
            )
            .map_err(|source| ReplayError::OrderBookError {
                sequence_num: event.sequence_num,
                source,
            })?,
            None => CommittedPrefix::default(),
        };
        if recorded.same_fills(&replayed) {
            return Ok(());
        }
        let first_divergent_trade =
            recorded
                .trades
                .iter()
                .zip(&replayed.trades)
                .position(|(a, b)| {
                    a.maker_order_id != b.maker_order_id
                        || a.price != b.price
                        || a.quantity != b.quantity
                });
        tracing::error!(
            sequence_num = event.sequence_num,
            recorded_trades = recorded.trades.len(),
            replayed_trades = replayed.trades.len(),
            recorded_executed = recorded.executed_quantity,
            replayed_executed = replayed.executed_quantity,
            first_divergent_trade = ?first_divergent_trade,
            "replay diverged: aborted submit committed a different prefix"
        );
        Err(ReplayError::OutcomeMismatch {
            sequence_num: event.sequence_num,
            recorded: RejectReason::MatchAborted,
            actual: Some(failure.error),
        })
    }

    /// Reconciles a dispatched submit's outcome against the journaled one.
    ///
    /// `recorded` is the reject code the journal carries when the live
    /// execution failed, `None` when it succeeded. Replay must reach the
    /// same verdict:
    ///
    /// - journaled success: the re-execution must succeed; an error is
    ///   [`ReplayError::OrderBookError`], as it always was;
    /// - journaled rejection: the re-execution must fail under the same
    ///   [`RejectReason`]; a success, or a failure under a different code,
    ///   is [`ReplayError::OutcomeMismatch`]. The fills the live command
    ///   made before failing are reproduced only if the verdicts agree, so
    ///   a disagreement means the reconstructed book has diverged.
    ///
    /// **Limitation:** only the code is compared. The error's details
    /// (`requested` / `available` on `InsufficientLiquidity`, say) and the
    /// fills behind the rejection are not, and a discrepancy confined to
    /// them can go undetected: different fills can exhaust the same levels
    /// and leave identical snapshots, so a later command is not guaranteed
    /// to expose it either. The same holds for a [`ReplayBookConfig`] that
    /// differs in a way the code cannot see — an STP mode that cancels the
    /// maker where the live one did not, both reporting
    /// `SelfTradePrevention`. That specific case is guarded separately by
    /// [`Self::check_stp_mode`], which compares the recorded mode rather
    /// than the code, but only journals carrying an STP rejection reach it.
    ///
    /// [`snapshots_match`] is the check that does catch a diverged book:
    /// it compares the reconstructed state field by field against a
    /// snapshot of the source book, which is what this reconciliation
    /// cannot do from the code alone. Run it after replay (or use
    /// [`ReplayEngine::verify`], which replays with default configuration
    /// and does exactly that). Matching the source book's configuration
    /// remains the caller's contract on every `*_with_config` entry point.
    fn reconcile_submit(
        event: &SequencerEvent<T>,
        recorded: Option<RejectReason>,
        outcome: Result<(), OrderBookError>,
    ) -> Result<(), ReplayError> {
        match (recorded, outcome) {
            (None, Ok(())) => Ok(()),
            (None, Err(source)) => Err(ReplayError::OrderBookError {
                sequence_num: event.sequence_num,
                source,
            }),
            (Some(code), Err(actual)) if RejectReason::from(&actual) == code => Ok(()),
            (Some(code), outcome) => Err(ReplayError::OutcomeMismatch {
                sequence_num: event.sequence_num,
                recorded: code,
                actual: outcome.err(),
            }),
        }
    }
}

/// Compares two [`OrderBookSnapshot`]s for structural equality.
///
/// Two snapshots are considered equal when:
/// - `symbol` is identical
/// - the pending trailing stops are identical, in admission order (current
///   stop price and watermark included), and so is the last trade price
///   they trail and trigger on (#286)
/// - The sorted bid price levels match, and the sorted ask price levels
///   match — where "match" means the **complete** per-level state (#208):
///   price, visible quantity, hidden quantity, order count, the full order
///   vector in queue-consumption order, and the deterministic execution
///   statistics.
///
/// This is the equality oracle for replay correctness. Aggregate-only
/// comparison (price / quantities / order count, #102) was a subset check:
/// two books with the same aggregates but reversed maker FIFO — or the same
/// FIFO with different maker ids, users, order variants, quantities, or
/// time-in-force — would emit different trades on the next sweep yet still
/// compare equal. Since pricelevel 0.9 the snapshot's `orders()` vector is
/// materialized in queue-consumption order, so element-wise [`crate::OrderType`]
/// equality pins maker identity and FIFO exactly.
///
/// Order equality includes the admission timestamp: the journal carries the
/// admitted order verbatim — timestamp baked in — and replay re-installs it
/// without re-stamping, so a faithful replay reproduces order timestamps
/// byte-identically (under any clock). Verifying against a book whose
/// orders were constructed and clock-stamped independently of the journal
/// will (correctly) report a mismatch.
///
/// Statistics comparison covers the deterministic counters — orders added /
/// removed / executed, quantity executed, value executed, and the sticky
/// `stats_degraded` flag. Intentionally excluded:
/// - `first_arrival_time` — before pricelevel 0.10 it came from a raw
///   `SystemTime::now()` at level creation; since 0.10 it starts at `0` and
///   is stamped from order timestamps, so it is deterministic, but it stays
///   excluded because snapshots written by older versions carry wall-clock
///   values;
/// - `last_execution_time` / `sum_waiting_time` — clock-derived, but live
///   ingestion and replay consume different clock-tick budgets by design
///   (the live submission API stamps each order with a fresh tick; replay
///   reuses the journal's pre-stamped order), so these wall-time aggregates
///   diverge even under identically-seeded injected clocks;
/// - the top-level snapshot capture timestamp, as before.
///
/// # Execution statistics and concurrent takers (#241)
///
/// The compared execution counters (`orders_executed`, `quantity_executed`,
/// `value_executed`, `stats_degraded`) are only coherent under pricelevel
/// 0.10's single-writer contract, and a live book lets shared-gate takers
/// sweep one level concurrently (see `OrderBook`'s "Level statistics are
/// advisory under concurrent takers"). The comparison stays exact anyway,
/// and is kept, for two reasons:
///
/// - **The replayed side has one writer.** Replay applies journal events
///   one at a time on the calling thread, so every level of the replayed
///   book has at most one `record_execution` in flight and its snapshot
///   never holds a partial execution.
/// - **The live side's totals are exact once quiescent.** Overlapping
///   recorders leave arithmetically exact final totals (atomic checked
///   updates, exact rollbacks), and the live book executes the same fills
///   per level as the replay, so a live snapshot taken with no sweep in
///   flight carries the same counters.
///
/// The caller's obligation is therefore to take `expected` while no sweep
/// is in flight on the live book, which a replay oracle needs regardless:
/// a snapshot captured mid-sweep does not correspond to any journal prefix
/// (its order vectors are mid-sweep too). A false divergence reported
/// against such a snapshot is a capture-timing error, not a replay fault;
/// dropping the counters would not make the comparison meaningful and would
/// hide a real under-count (`stats_degraded`) or a lost fill. One residual:
/// if a level's counters reach exhaustion, which execution gets dropped can
/// depend on the live interleaving, and the two sides may then legitimately
/// differ.
///
/// Note this tightening is a contract change for external consumers: two
/// independently built books with equal aggregates but different maker
/// identity or FIFO used to compare equal (pre-#208) and no longer do —
/// that laxity was the #102/#208 correctness gap, not a feature.
#[must_use]
pub fn snapshots_match(actual: &OrderBookSnapshot, expected: &OrderBookSnapshot) -> bool {
    if actual.symbol != expected.symbol {
        return false;
    }

    // #286: the pending trailing stops (element-wise, in admission order:
    // stop price and watermark included) and the last trade price they
    // trail on are book state a later trade depends on.
    actual.pending_stops == expected.pending_stops
        && actual.last_trade_price == expected.last_trade_price
        && sides_match(&actual.bids, &expected.bids)
        && sides_match(&actual.asks, &expected.asks)
}

/// Compares one side's levels, sorted ascending by price. The sort
/// direction is irrelevant for an equality check as long as both inputs use
/// the same one.
#[must_use]
fn sides_match(
    actual: &[pricelevel::PriceLevelSnapshot],
    expected: &[pricelevel::PriceLevelSnapshot],
) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    let mut actual_sorted: Vec<_> = actual.iter().collect();
    let mut expected_sorted: Vec<_> = expected.iter().collect();
    actual_sorted.sort_by_key(|level| level.price());
    expected_sorted.sort_by_key(|level| level.price());

    actual_sorted
        .iter()
        .zip(expected_sorted.iter())
        .all(|(a, b)| levels_match(a, b))
}

/// Complete per-level equality (#208): aggregates, the full order vector in
/// queue-consumption order, and the deterministic statistics counters.
#[must_use]
fn levels_match(a: &pricelevel::PriceLevelSnapshot, b: &pricelevel::PriceLevelSnapshot) -> bool {
    if a.price() != b.price()
        || a.visible_quantity() != b.visible_quantity()
        || a.hidden_quantity() != b.hidden_quantity()
        || a.order_count() != b.order_count()
    {
        return false;
    }

    // Element-wise order comparison in queue-consumption order (pricelevel
    // ≥ 0.9 materializes `orders()` that way). `OrderType`'s derived
    // `PartialEq` covers id, variant, side, price, visible / hidden
    // quantity, user, admission timestamp, time-in-force, and every
    // type-specific field (peg reference, trailing offset, replenish
    // config, ...).
    if a.orders() != b.orders() {
        return false;
    }

    // Deterministic statistics only — see the `snapshots_match` doc for the
    // intentionally excluded wall-clock-derived fields.
    let stats_a = a.statistics();
    let stats_b = b.statistics();
    stats_a.orders_added() == stats_b.orders_added()
        && stats_a.orders_removed() == stats_b.orders_removed()
        && stats_a.orders_executed() == stats_b.orders_executed()
        && stats_a.quantity_executed() == stats_b.quantity_executed()
        && stats_a.value_executed() == stats_b.value_executed()
        && stats_a.stats_degraded() == stats_b.stats_degraded()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orderbook::clock::{MonotonicClock, StubClock};
    use crate::orderbook::sequencer::InMemoryJournal;
    use crate::orderbook::trade::TradeResult;
    use pricelevel::{
        Hash32, Id, MatchResult, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs,
    };

    /// Fresh random order id (UUID v4).
    fn new_id() -> Id {
        Id::from_uuid(uuid::Uuid::new_v4())
    }

    fn make_add_event(seq: u64, id: Id, price: u128, qty: u64, side: Side) -> SequencerEvent<()> {
        let order = OrderType::Standard {
            id,
            price: Price::new(price),
            quantity: Quantity::new(qty),
            side,
            time_in_force: TimeInForce::Gtc,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        };
        SequencerEvent {
            sequence_num: seq,
            timestamp_ns: 0,
            command: SequencerCommand::AddOrder(order),
            result: SequencerResult::OrderAdded { order_id: id },
        }
    }

    /// #102: `snapshots_match` is the replay equality oracle and must compare the
    /// full per-level state — not just visible quantity. Two snapshots that differ
    /// only in hidden quantity or order count must NOT be reported equal.
    #[test]
    fn test_snapshots_match_compares_hidden_quantity_and_order_count() {
        fn lvl(
            price: u128,
            visible: u64,
            hidden: u64,
            count: usize,
        ) -> pricelevel::PriceLevelSnapshot {
            serde_json::from_value(serde_json::json!({
                "price": price,
                "visible_quantity": visible,
                "hidden_quantity": hidden,
                "order_count": count,
                "orders": []
            }))
            .expect("valid snapshot JSON")
        }

        let base = OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 0,
            bids: vec![lvl(100, 10, 5, 2)],
            asks: Vec::new(),
            pending_stops: Vec::new(),
            last_trade_price: None,
        };
        assert!(
            snapshots_match(&base, &base.clone()),
            "identical snapshots must match"
        );

        let diff_hidden = OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 0,
            bids: vec![lvl(100, 10, 7, 2)],
            asks: Vec::new(),
            pending_stops: Vec::new(),
            last_trade_price: None,
        };
        assert!(
            !snapshots_match(&base, &diff_hidden),
            "a hidden-quantity divergence must not be reported equal"
        );

        let diff_count = OrderBookSnapshot {
            symbol: "TEST".to_string(),
            timestamp: 0,
            bids: vec![lvl(100, 10, 5, 3)],
            asks: Vec::new(),
            pending_stops: Vec::new(),
            last_trade_price: None,
        };
        assert!(
            !snapshots_match(&base, &diff_count),
            "an order-count divergence must not be reported equal"
        );
    }

    /// Builds a one-level snapshot from real pricelevel levels for the #208
    /// oracle tests: `orders` are admitted in the given sequence, so the
    /// snapshot's order vector reflects exactly that FIFO.
    fn ask_level_snapshot(price: u128, orders: &[OrderType<()>]) -> pricelevel::PriceLevelSnapshot {
        let level = pricelevel::PriceLevel::new(price);
        for order in orders {
            assert!(
                level.add_order(*order).is_ok(),
                "fixture order must be admitted"
            );
        }
        level.snapshot().expect("level snapshot")
    }

    fn fixture_order(id: u64, price: u128, quantity: u64, tif: TimeInForce) -> OrderType<()> {
        OrderType::Standard {
            id: Id::from_u64(id),
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(1_700_000_000_000),
            time_in_force: tif,
            extra_fields: (),
        }
    }

    fn one_ask_snapshot(symbol: &str, level: pricelevel::PriceLevelSnapshot) -> OrderBookSnapshot {
        OrderBookSnapshot {
            symbol: symbol.to_string(),
            timestamp: 0,
            bids: Vec::new(),
            asks: vec![level],
            pending_stops: Vec::new(),
            last_trade_price: None,
        }
    }

    /// #208: equal aggregates with reversed maker FIFO must NOT compare
    /// equal — and the two snapshots demonstrably produce different first
    /// makers when restored and swept.
    #[test]
    fn test_snapshots_match_detects_reversed_fifo() {
        let first = fixture_order(1, 100, 10, TimeInForce::Gtc);
        let second = fixture_order(2, 100, 10, TimeInForce::Gtc);

        let forward = one_ask_snapshot("FIFO", ask_level_snapshot(100, &[first, second]));
        let reversed = one_ask_snapshot("FIFO", ask_level_snapshot(100, &[second, first]));

        assert!(
            snapshots_match(&forward, &forward.clone()),
            "identical FIFO must match"
        );
        assert!(
            !snapshots_match(&forward, &reversed),
            "reversed maker FIFO with equal aggregates must not match"
        );

        // The divergence is real: restoring each snapshot and sweeping one
        // unit consumes a different maker first.
        let sweep_first_maker = |snapshot: OrderBookSnapshot| -> Id {
            let book: OrderBook<()> = OrderBook::new("FIFO");
            book.restore_from_snapshot(snapshot).expect("restore");
            let result = book
                .submit_market_order_with_user(Id::from_u64(9_999), 1, Side::Buy, Hash32::zero())
                .expect("sweep");
            let trades = result.trades().as_vec();
            assert_eq!(trades.len(), 1, "one unit fills one maker");
            trades[0].maker_order_id()
        };
        let forward_maker = sweep_first_maker(forward);
        let reversed_maker = sweep_first_maker(reversed);
        assert_ne!(
            forward_maker, reversed_maker,
            "the previously-accepted snapshots produce different first makers"
        );
        assert_eq!(forward_maker, Id::from_u64(1));
        assert_eq!(reversed_maker, Id::from_u64(2));
    }

    /// #208: same aggregates, different order identity / fields — maker id,
    /// time-in-force — must not compare equal.
    #[test]
    fn test_snapshots_match_detects_order_identity_divergence() {
        let base = one_ask_snapshot(
            "IDENT",
            ask_level_snapshot(100, &[fixture_order(1, 100, 10, TimeInForce::Gtc)]),
        );

        let different_id = one_ask_snapshot(
            "IDENT",
            ask_level_snapshot(100, &[fixture_order(2, 100, 10, TimeInForce::Gtc)]),
        );
        assert!(
            !snapshots_match(&base, &different_id),
            "different maker id with equal aggregates must not match"
        );

        let different_tif = one_ask_snapshot(
            "IDENT",
            ask_level_snapshot(100, &[fixture_order(1, 100, 10, TimeInForce::Day)]),
        );
        assert!(
            !snapshots_match(&base, &different_tif),
            "different time-in-force with equal aggregates must not match"
        );
    }

    /// #208: a `stats_degraded` divergence is a replay-relevant signal (an
    /// under-counted statistics stream) and must not compare equal; the
    /// wall-clock statistics aggregates stay excluded.
    #[test]
    fn test_snapshots_match_compares_deterministic_statistics() {
        fn lvl_with_stats(degraded: bool, first_arrival: u64) -> pricelevel::PriceLevelSnapshot {
            serde_json::from_value(serde_json::json!({
                "price": 100,
                "visible_quantity": 10,
                "hidden_quantity": 0,
                "order_count": 0,
                "orders": [],
                "statistics": {
                    "orders_added": 1,
                    "orders_removed": 0,
                    "orders_executed": 0,
                    "quantity_executed": 0,
                    "value_executed": 0,
                    "last_execution_time": 0,
                    "first_arrival_time": first_arrival,
                    "sum_waiting_time": 0,
                    "stats_degraded": degraded
                }
            }))
            .expect("valid snapshot JSON")
        }

        let clean = OrderBookSnapshot {
            symbol: "STATS".to_string(),
            timestamp: 0,
            bids: vec![lvl_with_stats(false, 1_000)],
            asks: Vec::new(),
            pending_stops: Vec::new(),
            last_trade_price: None,
        };
        let degraded = OrderBookSnapshot {
            symbol: "STATS".to_string(),
            timestamp: 7,
            bids: vec![lvl_with_stats(true, 1_000)],
            asks: Vec::new(),
            pending_stops: Vec::new(),
            last_trade_price: None,
        };
        assert!(
            !snapshots_match(&clean, &degraded),
            "a stats_degraded divergence must not be reported equal"
        );

        // Wall-clock-derived statistics and the top-level capture timestamp
        // remain excluded: same deterministic state, different arrival time
        // and snapshot timestamp still match.
        let different_clock = OrderBookSnapshot {
            symbol: "STATS".to_string(),
            timestamp: 42,
            bids: vec![lvl_with_stats(false, 2_000)],
            asks: Vec::new(),
            pending_stops: Vec::new(),
            last_trade_price: None,
        };
        assert!(
            snapshots_match(&clean, &different_clock),
            "wall-clock-derived fields must stay excluded from the oracle"
        );
    }

    /// #126: the protocol sequence counter advances with `checked_add`, so at
    /// the `u64` boundary it surfaces a typed `SequenceOverflow` instead of
    /// silently stalling `expected_seq` (which would mask a real gap).
    #[test]
    fn test_replay_sequence_counter_overflow_is_a_typed_error() {
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        // A single event at the very top of the sequence space.
        let ev = make_add_event(u64::MAX, new_id(), 100, 10, Side::Buy);
        assert!(journal.append(&ev).is_ok());

        // Replaying from u64::MAX applies the event, then advancing the
        // expected-sequence counter past u64::MAX must overflow with a typed
        // error rather than saturating.
        match ReplayEngine::<()>::replay_from(&journal, u64::MAX, "TEST") {
            Err(ReplayError::SequenceOverflow { at }) => assert_eq!(at, u64::MAX),
            Err(other) => panic!("expected SequenceOverflow {{ at: u64::MAX }}, got {other:?}"),
            Ok(_) => panic!("advancing past u64::MAX must error"),
        }
    }

    #[test]
    fn test_replay_from_with_clock_uses_injected_clock() {
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        for (seq, price) in [(0u64, 100u128), (1, 101), (2, 102)] {
            let ev = make_add_event(seq, new_id(), price, 10, Side::Buy);
            assert!(journal.append(&ev).is_ok());
        }

        let clock: Arc<dyn Clock> = Arc::new(StubClock::starting_at(42_000));
        let result = ReplayEngine::<()>::replay_from_with_clock(&journal, 0, "TEST", clock);
        assert!(result.is_ok(), "replay_from_with_clock should succeed");
        let (book, last_seq) = result.expect("replay succeeded");
        assert_eq!(last_seq, 2);

        // The injected StubClock was seeded at 42_000. After the book has
        // been constructed, any ticks the replay consumed have advanced the
        // counter — so the next tick must be >= 42_000.
        let now = book.clock().now_millis();
        assert!(
            now.as_u64() >= 42_000,
            "expected injected clock value, got {}",
            now.as_u64()
        );
    }

    #[test]
    fn test_replay_from_with_clock_preserves_behavior_of_replay_from() {
        // Journal shared across both replays.
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        let ids: Vec<Id> = (0..3).map(|_| new_id()).collect();
        let events = [
            make_add_event(0, ids[0], 100, 5, Side::Buy),
            make_add_event(1, ids[1], 101, 7, Side::Buy),
            make_add_event(2, ids[2], 105, 3, Side::Sell),
        ];
        for ev in &events {
            assert!(journal.append(ev).is_ok());
        }

        let (book_plain, last_seq_plain) = ReplayEngine::<()>::replay_from(&journal, 0, "TEST")
            .expect("plain replay should succeed");

        let clock: Arc<dyn Clock> = Arc::new(MonotonicClock);
        let (book_with_clock, last_seq_with_clock) =
            ReplayEngine::<()>::replay_from_with_clock(&journal, 0, "TEST", clock)
                .expect("clock-aware replay should succeed");

        assert_eq!(last_seq_plain, last_seq_with_clock);
        assert_eq!(last_seq_plain, 2);

        let snap_plain = book_plain.create_snapshot(usize::MAX).expect("snapshot");
        let snap_with_clock = book_with_clock
            .create_snapshot(usize::MAX)
            .expect("snapshot");
        assert!(
            snapshots_match(&snap_plain, &snap_with_clock),
            "snapshots must match across replay variants"
        );
    }

    #[test]
    fn test_replay_from_with_clock_propagates_sequence_gap() {
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        // Sequences 0, 1, 2, then jump to 4 (gap at 3).
        let events = [
            make_add_event(0, new_id(), 100, 1, Side::Buy),
            make_add_event(1, new_id(), 101, 1, Side::Buy),
            make_add_event(2, new_id(), 102, 1, Side::Buy),
            make_add_event(4, new_id(), 104, 1, Side::Buy),
        ];
        for ev in &events {
            assert!(journal.append(ev).is_ok());
        }

        let clock: Arc<dyn Clock> = Arc::new(StubClock::new());
        let result = ReplayEngine::<()>::replay_from_with_clock(&journal, 0, "TEST", clock);

        match result {
            Err(ReplayError::SequenceGap { expected, found }) => {
                assert_eq!(expected, 3);
                assert_eq!(found, 4);
            }
            Err(other) => panic!(
                "expected SequenceGap {{ expected: 3, found: 4 }}, got {:?}",
                other
            ),
            Ok(_) => panic!("expected SequenceGap {{ expected: 3, found: 4 }}, got Ok(_)"),
        }
    }

    /// A journal whose iterator stops before the last sequence it reports,
    /// as a `FileJournal` does when its newest segment file vanished.
    struct TruncatingJournal {
        inner: InMemoryJournal<()>,
        reported_last: u64,
    }

    impl Journal<()> for TruncatingJournal {
        fn append(&self, event: &SequencerEvent<()>) -> Result<(), JournalError> {
            self.inner.append(event)
        }
        fn read_from(
            &self,
            sequence: u64,
        ) -> Result<crate::orderbook::sequencer::JournalReadIter<()>, JournalError> {
            self.inner.read_from(sequence)
        }
        fn last_sequence(&self) -> Result<Option<u64>, JournalError> {
            Ok(Some(self.reported_last))
        }
        fn verify_integrity(&self) -> Result<(), JournalError> {
            Ok(())
        }
    }

    /// #295: an iteration that ends before `last_sequence()` is a typed
    /// error, never `Ok` on a truncated prefix.
    #[test]
    fn test_replay_reports_journal_truncated_before_last_sequence() {
        let journal = TruncatingJournal {
            inner: InMemoryJournal::new(),
            reported_last: 5,
        };
        for seq in 0..3 {
            journal
                .append(&make_add_event(
                    seq,
                    new_id(),
                    100 + u128::from(seq),
                    1,
                    Side::Buy,
                ))
                .expect("append");
        }
        match ReplayEngine::<()>::replay_from(&journal, 0, "TEST") {
            Err(ReplayError::JournalTruncated {
                expected_last,
                reached,
            }) => assert_eq!((expected_last, reached), (5, Some(2))),
            other => panic!("expected JournalTruncated, got {:?}", other.err()),
        }
        // A suffix replay whose start is past every readable entry reads
        // nothing and still fails.
        match ReplayEngine::<()>::replay_from(&journal, 4, "TEST") {
            Err(ReplayError::JournalTruncated {
                expected_last,
                reached,
            }) => assert_eq!((expected_last, reached), (5, None)),
            other => panic!("expected JournalTruncated, got {:?}", other.err()),
        }
        let err = ReplayError::JournalTruncated {
            expected_last: 5,
            reached: None,
        };
        assert!(err.to_string().contains("no entry"));
    }

    #[test]
    fn test_replay_complete_journal_is_not_truncated() {
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        for seq in 0..3 {
            journal
                .append(&make_add_event(
                    seq,
                    new_id(),
                    100 + u128::from(seq),
                    1,
                    Side::Buy,
                ))
                .expect("append");
        }
        let (_, last) = ReplayEngine::<()>::replay_from(&journal, 1, "TEST").expect("replay");
        assert_eq!(last, 2);
    }

    #[test]
    fn test_replay_market_order_by_amount_matches_live_book() {
        // Build a journal: seed an ask wall, then take it with a
        // notional market order. Replay against a fresh book and
        // require the resulting snapshot to match the live one — proves
        // the additive variant dispatches identically to the live path.
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        let mut seq = 0u64;

        // Three asks at 100, 101, 102 — each size 10. The ids are shared
        // with the live book below: since #208 `snapshots_match` compares
        // full order identity and FIFO, the ground-truth book must be
        // seeded with the exact orders the journal carries.
        let maker_ids = [Id::from_u64(1), Id::from_u64(2), Id::from_u64(3)];
        for (maker_id, price) in maker_ids.iter().zip([100u128, 101, 102]) {
            let ev = make_add_event(seq, *maker_id, price, 10, Side::Sell);
            assert!(journal.append(&ev).is_ok());
            seq += 1;
        }

        // Notional buy: $1500 sweeps 10@100 + 10@101 = $2010 total — but
        // we cap at $1500 so only 10@100 + 4@101 (=$1404) lands. The
        // residual $96 is dust < 1*101 = 101 still — actually it can buy
        // 0 more at 101 → stop short of the third level. Exact behavior
        // doesn't matter for this test; what matters is replay parity.
        let taker_id = new_id();
        let ev = SequencerEvent::<()> {
            sequence_num: seq,
            timestamp_ns: 0,
            command: SequencerCommand::MarketOrderByAmount {
                id: taker_id,
                amount: 1_500,
                side: Side::Buy,
            },
            // Result is informational for replay — replay re-executes
            // the command against a fresh book. Use TradeExecuted with an
            // empty match-result so the journal entry stays semantically
            // consistent with a market-by-amount taker (and is not skipped
            // by the Rejected branch in `replay_from`).
            result: SequencerResult::TradeExecuted {
                trade_result: TradeResult::new(
                    "TEST".to_string(),
                    MatchResult::new(taker_id, Quantity::new(0)),
                )
                .expect("valid trade result"),
            },
        };
        assert!(journal.append(&ev).is_ok());

        // Drive the live book through the same sequence so we have a
        // ground-truth snapshot.
        let live_book: crate::OrderBook<()> = crate::OrderBook::new("TEST");
        for (maker_id, price) in maker_ids.iter().zip([100u128, 101, 102]) {
            live_book
                .add_order(OrderType::Standard {
                    id: *maker_id,
                    price: Price::new(price),
                    quantity: Quantity::new(10),
                    side: Side::Sell,
                    time_in_force: TimeInForce::Gtc,
                    user_id: Hash32::zero(),
                    timestamp: TimestampMs::new(0),
                    extra_fields: (),
                })
                .expect("seed ask");
        }
        let _ = live_book.match_market_order_by_amount(taker_id, 1_500, Side::Buy);

        // Replay journal into a fresh book.
        let (replayed, last_seq) =
            ReplayEngine::<()>::replay_from(&journal, 0, "TEST").expect("replay must succeed");
        assert_eq!(last_seq, seq);

        let live_snap = live_book.create_snapshot(usize::MAX).expect("snapshot");
        let replayed_snap = replayed.create_snapshot(usize::MAX).expect("snapshot");
        assert!(
            snapshots_match(&live_snap, &replayed_snap),
            "live and replayed snapshots must match after notional market order"
        );
    }

    /// #101: `replay_from_with_config` applies every config field to the fresh
    /// book before replaying. A `Default` config leaves the book at defaults;
    /// a populated config is reflected field-for-field.
    #[test]
    fn test_replay_from_with_config_applies_every_field() {
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        let ev = make_add_event(0, new_id(), 100, 10, Side::Buy);
        assert!(journal.append(&ev).is_ok());

        // Default config => all-defaults book.
        let (book, _) = ReplayEngine::<()>::replay_from_with_config(
            &journal,
            0,
            "TEST",
            &ReplayBookConfig::default(),
        )
        .expect("default config replay");
        assert_eq!(book.fee_schedule(), None);
        assert_eq!(book.stp_mode(), STPMode::None);
        assert_eq!(book.tick_size(), None);
        assert_eq!(book.lot_size(), None);
        assert_eq!(book.min_order_size(), None);
        assert_eq!(book.max_order_size(), None);

        // Populated config => reflected on the reconstructed book. Price 100 is
        // on the 10-tick grid and qty 10 is a 5-lot multiple within [1, 1000].
        let fee = FeeSchedule::new(-2, 5);
        let config = ReplayBookConfig::new(
            Some(fee),
            STPMode::None,
            Some(10),
            Some(5),
            Some(1),
            Some(1_000),
        );
        let (book, _) = ReplayEngine::<()>::replay_from_with_config(&journal, 0, "TEST", &config)
            .expect("populated config replay");
        assert_eq!(book.fee_schedule(), Some(fee));
        assert_eq!(book.tick_size(), Some(10));
        assert_eq!(book.lot_size(), Some(5));
        assert_eq!(book.min_order_size(), Some(1));
        assert_eq!(book.max_order_size(), Some(1_000));
    }

    /// #101: the `*_with_config` variants share the pre-checks of the plain
    /// entry points — an empty journal is `EmptyJournal`, an out-of-range
    /// `from_sequence` is `InvalidSequence`.
    #[test]
    fn test_replay_with_config_pre_checks_match_plain_variants() {
        let empty: InMemoryJournal<()> = InMemoryJournal::new();
        assert!(matches!(
            ReplayEngine::<()>::replay_from_with_config(
                &empty,
                0,
                "TEST",
                &ReplayBookConfig::default()
            ),
            Err(ReplayError::EmptyJournal)
        ));

        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        let ev = make_add_event(0, new_id(), 100, 10, Side::Buy);
        assert!(journal.append(&ev).is_ok());
        let clock: Arc<dyn Clock> = Arc::new(StubClock::new());
        match ReplayEngine::<()>::replay_from_with_clock_and_config(
            &journal,
            5,
            "TEST",
            clock,
            &ReplayBookConfig::default(),
        ) {
            Err(ReplayError::InvalidSequence {
                from_sequence,
                last_sequence,
            }) => {
                assert_eq!(from_sequence, 5);
                assert_eq!(last_sequence, 0);
            }
            Err(other) => panic!("expected InvalidSequence, got {other:?}"),
            Ok(_) => panic!("expected InvalidSequence, got Ok(_)"),
        }
    }

    #[test]
    fn test_market_order_by_amount_command_serde_json_roundtrip() {
        let cmd: SequencerCommand<()> = SequencerCommand::MarketOrderByAmount {
            id: new_id(),
            amount: 12_345_678,
            side: Side::Buy,
        };
        let json = serde_json::to_vec(&cmd).expect("serialize");
        let decoded: SequencerCommand<()> = serde_json::from_slice(&json).expect("deserialize");
        match decoded {
            SequencerCommand::MarketOrderByAmount { amount, side, .. } => {
                assert_eq!(amount, 12_345_678);
                assert_eq!(side, Side::Buy);
            }
            other => panic!("expected MarketOrderByAmount, got {other:?}"),
        }
    }

    #[cfg(feature = "bincode")]
    #[test]
    fn test_market_order_by_amount_command_bincode_roundtrip() {
        use bincode::config::standard;
        use bincode::serde::{decode_from_slice, encode_to_vec};
        let cmd: SequencerCommand<()> = SequencerCommand::MarketOrderByAmount {
            id: new_id(),
            amount: 999_999,
            side: Side::Sell,
        };
        let bytes = encode_to_vec(&cmd, standard()).expect("encode");
        let (decoded, n) =
            decode_from_slice::<SequencerCommand<()>, _>(&bytes, standard()).expect("decode");
        assert_eq!(n, bytes.len());
        match decoded {
            SequencerCommand::MarketOrderByAmount { amount, side, .. } => {
                assert_eq!(amount, 999_999);
                assert_eq!(side, Side::Sell);
            }
            other => panic!("expected MarketOrderByAmount, got {other:?}"),
        }
    }

    /// #189: the appended `EvictExpiredOrders` command round-trips through
    /// JSON — the `now_ms` cutoff decodes byte-identically. `TimestampMs` is
    /// `#[serde(transparent)]`, so it encodes as a bare millisecond count.
    #[test]
    fn test_evict_expired_orders_command_serde_json_roundtrip() {
        let cmd: SequencerCommand<()> = SequencerCommand::EvictExpiredOrders {
            now_ms: TimestampMs::new(1_700_000_000_000),
        };
        let json = serde_json::to_vec(&cmd).expect("serialize");
        let decoded: SequencerCommand<()> = serde_json::from_slice(&json).expect("deserialize");
        match decoded {
            SequencerCommand::EvictExpiredOrders { now_ms } => {
                assert_eq!(now_ms, TimestampMs::new(1_700_000_000_000));
            }
            other => panic!("expected EvictExpiredOrders, got {other:?}"),
        }
    }

    /// #189: the appended `EvictExpiredOrders` command round-trips through
    /// bincode with no trailing bytes. Because the variant is appended after
    /// every prior variant, old journals keep their bincode variant indices.
    #[cfg(feature = "bincode")]
    #[test]
    fn test_evict_expired_orders_command_bincode_roundtrip() {
        use bincode::config::standard;
        use bincode::serde::{decode_from_slice, encode_to_vec};
        let cmd: SequencerCommand<()> = SequencerCommand::EvictExpiredOrders {
            now_ms: TimestampMs::new(42_000),
        };
        let bytes = encode_to_vec(&cmd, standard()).expect("encode");
        let (decoded, n) =
            decode_from_slice::<SequencerCommand<()>, _>(&bytes, standard()).expect("decode");
        assert_eq!(n, bytes.len());
        match decoded {
            SequencerCommand::EvictExpiredOrders { now_ms } => {
                assert_eq!(now_ms, TimestampMs::new(42_000));
            }
            other => panic!("expected EvictExpiredOrders, got {other:?}"),
        }
    }

    /// #189: an `EvictExpiredOrders` command replays deterministically. Drive a
    /// live book through a set of GTD / GTC adds plus a sweep, journaling each
    /// command; replay the journal into a fresh book (with a matching logical
    /// clock so the small GTD deadlines re-admit) and require the post-sweep
    /// state to match the live one. `snapshots_match` is the oracle. The sweep
    /// consumes the journaled `now_ms`, never the replay clock — that is the
    /// determinism contract for the variant.
    #[test]
    fn test_replay_evict_expired_orders_matches_live_book() {
        fn order(id: Id, price: u128, qty: u64, side: Side, tif: TimeInForce) -> OrderType<()> {
            OrderType::Standard {
                id,
                price: Price::new(price),
                quantity: Quantity::new(qty),
                side,
                time_in_force: tif,
                user_id: Hash32::zero(),
                timestamp: TimestampMs::new(0),
                extra_fields: (),
            }
        }

        let symbol = "TEST";
        let journal: InMemoryJournal<()> = InMemoryJournal::new();

        // Two GTD orders expire at t=1_000; one GTD rests until t=10_000; a GTC
        // order never expires. Built once so the live book and the journal carry
        // identical AddOrder commands.
        let expiring_bid = order(new_id(), 100, 5, Side::Buy, TimeInForce::Gtd(1_000));
        let future_bid = order(new_id(), 99, 3, Side::Buy, TimeInForce::Gtd(10_000));
        let gtc_bid = order(new_id(), 98, 4, Side::Buy, TimeInForce::Gtc);
        let expiring_ask = order(new_id(), 101, 7, Side::Sell, TimeInForce::Gtd(1_000));
        let orders = [expiring_bid, future_bid, gtc_bid, expiring_ask];

        // Live book on a logical clock so the small deadlines admit (wall-clock
        // admission would treat them as already expired).
        let clock_live: Arc<dyn Clock> = Arc::new(StubClock::starting_at(0));
        let live = OrderBook::<()>::with_clock(symbol, clock_live);

        let mut seq = 0u64;
        for ord in &orders {
            live.add_order(*ord).expect("live add");
            let ev = SequencerEvent::<()> {
                sequence_num: seq,
                timestamp_ns: 0,
                command: SequencerCommand::AddOrder(*ord),
                result: SequencerResult::OrderAdded { order_id: ord.id() },
            };
            assert!(journal.append(&ev).is_ok());
            seq += 1;
        }

        // Sweep live at t=5_000: evicts the two t=1_000 orders, keeps the rest.
        let now = TimestampMs::new(5_000);
        let evicted = live.evict_expired_orders(now).expect("evict");
        assert_eq!(evicted.len(), 2, "two GTD orders expire by t=5_000");
        let sweep = SequencerEvent::<()> {
            sequence_num: seq,
            timestamp_ns: 0,
            command: SequencerCommand::EvictExpiredOrders { now_ms: now },
            result: SequencerResult::MassCancelled {
                result: evicted.mass_cancel_result().clone(),
            },
        };
        assert!(journal.append(&sweep).is_ok());

        // Replay with a matching logical clock so AddOrder re-admissions succeed;
        // the sweep applies the journaled cutoff, not the clock.
        let clock_replay: Arc<dyn Clock> = Arc::new(StubClock::starting_at(0));
        let (replayed, last_seq) =
            ReplayEngine::<()>::replay_from_with_clock(&journal, 0, symbol, clock_replay)
                .expect("replay must succeed");
        assert_eq!(last_seq, seq);

        let live_snap = live.create_snapshot(usize::MAX).expect("snapshot");
        let replayed_snap = replayed.create_snapshot(usize::MAX).expect("snapshot");
        assert!(
            snapshots_match(&live_snap, &replayed_snap),
            "post-sweep live and replayed snapshots must match"
        );

        // Sanity: the expired levels are gone, the survivors remain.
        assert_eq!(replayed_snap.bids.len(), 2, "99 and 98 bids survive");
        assert!(replayed_snap.asks.is_empty(), "the only ask expired");
    }

    fn gtd_order(id: u64, price: u128, side: Side) -> OrderType<()> {
        OrderType::Standard {
            id: Id::from_u64(id),
            price: Price::new(price),
            quantity: Quantity::new(5),
            side,
            time_in_force: TimeInForce::Gtd(1_000),
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        }
    }

    /// Journal of three GTD admissions on a logical-clock live book whose
    /// cancel of id 2 is refused by its level (#248).
    fn partial_eviction_fixture() -> (OrderBook<()>, InMemoryJournal<()>) {
        use crate::orderbook::book::CancelFault;

        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        let clock: Arc<dyn Clock> = Arc::new(StubClock::starting_at(0));
        let mut live = OrderBook::<()>::with_clock("PEVICT", clock);
        live.cancel_fault_hook = Some(Arc::new(|id| {
            (id == Id::from_u64(2)).then(|| {
                CancelFault::Refuse(pricelevel::PriceLevelError::InvalidOperation {
                    message: "refused".to_string(),
                })
            })
        }));
        for (seq, ord) in [
            gtd_order(1, 100, Side::Buy),
            gtd_order(2, 101, Side::Buy),
            gtd_order(3, 110, Side::Sell),
        ]
        .into_iter()
        .enumerate()
        {
            live.add_order(ord).expect("live add");
            assert!(
                journal
                    .append(&SequencerEvent::<()> {
                        sequence_num: u64::try_from(seq).expect("seq"),
                        timestamp_ns: 0,
                        command: SequencerCommand::AddOrder(ord),
                        result: SequencerResult::OrderAdded { order_id: ord.id() },
                    })
                    .is_ok()
            );
        }
        (live, journal)
    }

    /// #248: a partial live eviction (one expired order refused by its
    /// level) replays faithfully: replay evicts exactly the journaled ids,
    /// so the order the live sweep failed to evict keeps resting.
    #[test]
    fn test_replay_partial_eviction_matches_live_book() {
        let (live, journal) = partial_eviction_fixture();
        let now = TimestampMs::new(5_000);
        let swept = live.evict_expired_orders(now).expect("sweep ran");
        assert_eq!(
            swept.evicted_order_ids(),
            &[Id::from_u64(1), Id::from_u64(3)]
        );
        assert!(swept.has_failures());
        assert!(
            journal
                .append(&SequencerEvent::<()> {
                    sequence_num: 3,
                    timestamp_ns: 0,
                    command: SequencerCommand::EvictExpiredOrders { now_ms: now },
                    result: SequencerResult::MassCancelled {
                        result: swept.into_mass_cancel_result(),
                    },
                })
                .is_ok()
        );

        // The replay book has no fault hook: re-running the sweep would
        // evict id 2 as well.
        let clock: Arc<dyn Clock> = Arc::new(StubClock::starting_at(0));
        let (replayed, last_seq) =
            ReplayEngine::<()>::replay_from_with_clock(&journal, 0, "PEVICT", clock)
                .expect("replay must succeed");
        assert_eq!(last_seq, 3);
        let live_snap = live.create_snapshot(usize::MAX).expect("snapshot");
        let replayed_snap = replayed.create_snapshot(usize::MAX).expect("snapshot");
        assert!(snapshots_match(&live_snap, &replayed_snap));
        assert!(replayed.get_order(Id::from_u64(2)).is_some());
        assert!(replayed.get_order(Id::from_u64(1)).is_none());
    }

    /// Journals `command` with the result its live execution produced, the
    /// way a sequencer does (`SequencerResult::from(&err)` on `Err`).
    fn journal_live(
        journal: &InMemoryJournal<()>,
        seq: u64,
        command: SequencerCommand<()>,
        ok: SequencerResult,
        outcome: Result<(), OrderBookError>,
    ) {
        let result = match outcome {
            Ok(()) => ok,
            Err(err) => SequencerResult::from(&err),
        };
        assert!(
            journal
                .append(&SequencerEvent::<()> {
                    sequence_num: seq,
                    timestamp_ns: 0,
                    command,
                    result,
                })
                .is_ok()
        );
    }

    /// Live book with bids 1 @ 100 and 2 @ 101 (journaled), whose cancel of
    /// id 1 is committed by its level and then fails (#248).
    fn level_fault_fixture(symbol: &str) -> (OrderBook<()>, InMemoryJournal<()>) {
        use crate::orderbook::book::CancelFault;

        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        let mut live = OrderBook::<()>::new(symbol);
        live.cancel_fault_hook = Some(Arc::new(|id| {
            (id == Id::from_u64(1)).then(|| {
                CancelFault::RemoveThenFail(pricelevel::PriceLevelError::InvalidOperation {
                    message: "broken level".to_string(),
                })
            })
        }));
        for (seq, (id, price)) in [(1u64, 100u128), (2, 101)].into_iter().enumerate() {
            let ev = make_add_event(
                u64::try_from(seq).expect("seq"),
                Id::from_u64(id),
                price,
                5,
                Side::Buy,
            );
            if let SequencerCommand::AddOrder(order) = &ev.command {
                live.add_order(*order).expect("live add");
            }
            assert!(journal.append(&ev).is_ok());
        }
        (live, journal)
    }

    fn assert_replay_matches(live: &OrderBook<()>, journal: &InMemoryJournal<()>, symbol: &str) {
        let (replayed, _) =
            ReplayEngine::<()>::replay_from(journal, 0, symbol).expect("replay must succeed");
        let live_snap = live.create_snapshot(usize::MAX).expect("snapshot");
        let replayed_snap = replayed.create_snapshot(usize::MAX).expect("snapshot");
        assert!(
            snapshots_match(&live_snap, &replayed_snap),
            "replayed book diverged from the live one"
        );
    }

    /// #248: `SequencerResult::from` records a level-committed removal as
    /// the cancel it was, not as a rejection.
    #[test]
    fn test_sequencer_result_records_level_fault_removal_as_cancel() {
        let err = OrderBookError::OrderRemovedWithLevelFault {
            order_id: Id::from_u64(5),
            source: Box::new(pricelevel::PriceLevelError::InvalidOperation {
                message: "broken level".to_string(),
            }),
        };
        assert!(matches!(
            SequencerResult::from(&err),
            SequencerResult::OrderCancelled { order_id } if order_id == Id::from_u64(5)
        ));
    }

    /// #248: a cancel whose level committed the removal and then failed is
    /// journaled as the cancel it was, and replay removes the order too.
    #[test]
    fn test_replay_cancel_committed_by_a_faulty_level_matches_live_book() {
        let (live, journal) = level_fault_fixture("LFAULT");
        let outcome = live.cancel_order(Id::from_u64(1)).map(|_| ());
        assert!(matches!(
            outcome,
            Err(OrderBookError::OrderRemovedWithLevelFault { .. })
        ));
        journal_live(
            &journal,
            2,
            SequencerCommand::CancelOrder(Id::from_u64(1)),
            SequencerResult::OrderCancelled {
                order_id: Id::from_u64(1),
            },
            outcome,
        );
        assert_replay_matches(&live, &journal, "LFAULT");
    }

    /// #248: a re-price whose cancel the level committed before failing
    /// removed the original and re-added nothing; replay applies the
    /// journaled cancel instead of re-executing the update (which would
    /// re-add it).
    #[test]
    fn test_replay_modify_whose_cancel_hit_a_faulty_level_matches_live_book() {
        let (live, journal) = level_fault_fixture("LFMOD");
        let update = pricelevel::OrderUpdate::UpdatePrice {
            order_id: Id::from_u64(1),
            new_price: Price::new(99),
        };
        let outcome = live.update_order(update).map(|_| ());
        assert!(matches!(
            outcome,
            Err(OrderBookError::OrderRemovedWithLevelFault { .. })
        ));
        assert!(live.get_order(Id::from_u64(1)).is_none(), "original gone");
        journal_live(
            &journal,
            2,
            SequencerCommand::UpdateOrder(update),
            SequencerResult::OrderUpdated {
                order_id: Id::from_u64(1),
            },
            outcome,
        );
        assert_replay_matches(&live, &journal, "LFMOD");
    }

    /// #248: a journaled eviction naming an order the replay book does not
    /// hold is a divergence, reported instead of skipped.
    #[test]
    fn test_replay_eviction_of_missing_order_is_reported() {
        let (_live, journal) = partial_eviction_fixture();
        let claimed = MassCancelResult::new(2, vec![Id::from_u64(1), Id::from_u64(77)]);
        assert!(
            journal
                .append(&SequencerEvent::<()> {
                    sequence_num: 3,
                    timestamp_ns: 0,
                    command: SequencerCommand::EvictExpiredOrders {
                        now_ms: TimestampMs::new(5_000),
                    },
                    result: SequencerResult::MassCancelled { result: claimed },
                })
                .is_ok()
        );
        let clock: Arc<dyn Clock> = Arc::new(StubClock::starting_at(0));
        match ReplayEngine::<()>::replay_from_with_clock(&journal, 0, "PEVICT", clock) {
            Err(ReplayError::MassCancelMismatch {
                sequence_num: 3,
                divergence,
                recorded,
                replayed,
            }) => {
                assert_eq!(
                    divergence,
                    MassCancelDivergence::CancelledIds {
                        position: 1,
                        recorded_count: 2,
                        replayed_count: 1,
                    }
                );
                assert_eq!(
                    recorded.cancelled_order_ids(),
                    &[Id::from_u64(1), Id::from_u64(77)]
                );
                assert_eq!(replayed.cancelled_order_ids(), &[Id::from_u64(1)]);
            }
            Err(other) => panic!("expected MassCancelMismatch, got {other:?}"),
            Ok(_) => panic!("a diverged eviction must not replay silently"),
        }
    }

    /// A mass cancel journaled as refused cancelled nothing live, so replay
    /// must not re-execute it even when the replay book is readable.
    #[test]
    fn test_replay_skips_mass_cancel_recorded_as_refused() {
        use crate::orderbook::mass_cancel::MassCancelFailure;

        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        let symbol = "REFUSED";
        let live = OrderBook::<()>::new(symbol);
        let order = OrderType::Standard {
            id: Id::from_u64(1),
            price: Price::new(100),
            quantity: Quantity::new(5),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };
        live.add_order(order).expect("live add");
        assert!(
            journal
                .append(&SequencerEvent::<()> {
                    sequence_num: 0,
                    timestamp_ns: 0,
                    command: SequencerCommand::AddOrder(order),
                    result: SequencerResult::OrderAdded {
                        order_id: order.id()
                    },
                })
                .is_ok()
        );
        let refused = MassCancelResult::refused(MassCancelFailure::LevelUnreadable {
            side: Side::Buy,
            price: 100,
            error: pricelevel::PriceLevelError::InvalidOperation {
                message: "unreadable".to_string(),
            },
        });
        assert!(
            journal
                .append(&SequencerEvent::<()> {
                    sequence_num: 1,
                    timestamp_ns: 0,
                    command: SequencerCommand::CancelAll,
                    result: SequencerResult::MassCancelled { result: refused },
                })
                .is_ok()
        );

        let (replayed, last_seq) =
            ReplayEngine::<()>::replay_from(&journal, 0, symbol).expect("replay must succeed");
        assert_eq!(last_seq, 1);
        let live_snap = live.create_snapshot(usize::MAX).expect("snapshot");
        let replayed_snap = replayed.create_snapshot(usize::MAX).expect("snapshot");
        assert!(snapshots_match(&live_snap, &replayed_snap));
        assert_eq!(
            replayed_snap.bids.len(),
            1,
            "the refused cancel kept the order"
        );
    }

    /// #248 / #252: a mass cancel journaled with per-order failures is
    /// partial, not refused. Replay must not skip it (its listed ids were
    /// cancelled live) and reconciles it by identity: the fresh replay book
    /// cancels the order the live level refused, so the ids diverge.
    #[test]
    fn test_replay_reports_mass_cancel_recorded_as_partial() {
        use crate::orderbook::mass_cancel::MassCancelFailure;

        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        let symbol = "PARTIAL";
        for (seq, id) in [(0u64, 1u64), (1, 2)] {
            assert!(
                journal
                    .append(&make_add_event(seq, Id::from_u64(id), 100, 5, Side::Buy))
                    .is_ok()
            );
        }
        let partial = MassCancelResult::with_failures(
            vec![Id::from_u64(1)],
            vec![MassCancelFailure::OrderCancelFailed {
                order_id: Id::from_u64(2),
                error: pricelevel::PriceLevelError::InvalidOperation {
                    message: "refused".to_string(),
                },
            }],
        );
        assert!(partial.has_failures() && !partial.is_refused());
        assert!(
            journal
                .append(&SequencerEvent::<()> {
                    sequence_num: 2,
                    timestamp_ns: 0,
                    command: SequencerCommand::CancelBySide { side: Side::Buy },
                    result: SequencerResult::MassCancelled { result: partial },
                })
                .is_ok()
        );

        match ReplayEngine::<()>::replay_from(&journal, 0, symbol) {
            Err(ReplayError::MassCancelMismatch {
                sequence_num,
                divergence,
                ..
            }) => {
                assert_eq!(sequence_num, 2);
                assert_eq!(
                    divergence,
                    MassCancelDivergence::CancelledIds {
                        position: 1,
                        recorded_count: 1,
                        replayed_count: 2,
                    }
                );
            }
            Err(other) => panic!("expected MassCancelMismatch, got {other:?}"),
            Ok(_) => panic!("a partial mass cancel must not replay silently"),
        }
    }

    // --- mass cancel identity reconciliation (#252) -------------------------

    /// Journal (and live book) holding bids 1 @ 100, 2 @ 100 and 3 @ 101.
    fn three_bids_fixture(symbol: &str) -> (OrderBook<()>, InMemoryJournal<()>) {
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        let live = OrderBook::<()>::new(symbol);
        for (seq, (id, price)) in [(1u64, 100u128), (2, 100), (3, 101)]
            .into_iter()
            .enumerate()
        {
            let ev = make_add_event(
                u64::try_from(seq).expect("seq"),
                Id::from_u64(id),
                price,
                5,
                Side::Buy,
            );
            if let SequencerCommand::AddOrder(order) = &ev.command {
                live.add_order(*order).expect("live add");
            }
            assert!(journal.append(&ev).is_ok());
        }
        (live, journal)
    }

    fn append_mass_cancel(
        journal: &InMemoryJournal<()>,
        seq: u64,
        command: SequencerCommand<()>,
        result: MassCancelResult,
    ) {
        assert!(
            journal
                .append(&SequencerEvent::<()> {
                    sequence_num: seq,
                    timestamp_ns: 0,
                    command,
                    result: SequencerResult::MassCancelled { result },
                })
                .is_ok()
        );
    }

    /// #252: equal counts can hide different cancelled orders. A journaled
    /// result naming the right number of orders but the wrong ones (or the
    /// right ones in the wrong order) is a mismatch, not a success.
    #[test]
    fn test_replay_mass_cancel_equal_count_different_ids_is_mismatch() {
        for (claimed, position) in [
            // Same count, one foreign id.
            (vec![Id::from_u64(1), Id::from_u64(2), Id::from_u64(99)], 2),
            // Same ids, different order.
            (vec![Id::from_u64(2), Id::from_u64(1), Id::from_u64(3)], 0),
        ] {
            let (_live, journal) = three_bids_fixture("IDENT");
            append_mass_cancel(
                &journal,
                3,
                SequencerCommand::CancelBySide { side: Side::Buy },
                MassCancelResult::new(3, claimed),
            );
            match ReplayEngine::<()>::replay_from(&journal, 0, "IDENT") {
                Err(ReplayError::MassCancelMismatch {
                    sequence_num: 3,
                    divergence,
                    ..
                }) => assert_eq!(
                    divergence,
                    MassCancelDivergence::CancelledIds {
                        position,
                        recorded_count: 3,
                        replayed_count: 3,
                    }
                ),
                Err(other) => panic!("expected MassCancelMismatch, got {other:?}"),
                Ok(_) => panic!("an identity mismatch must not replay silently"),
            }
        }
    }

    /// #252: a journal that recorded fewer cancelled orders than the replay
    /// cancels (the historical `MassCancelResult::default()` placeholder) is
    /// a mismatch.
    #[test]
    fn test_replay_mass_cancel_recorded_empty_but_replay_cancels_is_mismatch() {
        let (_live, journal) = three_bids_fixture("EMPTYREC");
        append_mass_cancel(
            &journal,
            3,
            SequencerCommand::CancelAll,
            MassCancelResult::default(),
        );
        let err = ReplayEngine::<()>::replay_from(&journal, 0, "EMPTYREC")
            .err()
            .expect("must diverge");
        assert!(matches!(
            err,
            ReplayError::MassCancelMismatch {
                divergence: MassCancelDivergence::CancelledIds {
                    position: 0,
                    recorded_count: 0,
                    replayed_count: 3,
                },
                ..
            }
        ));
        assert!(
            err.to_string()
                .contains("cancelled ids differ at position 0")
        );
    }

    /// #252: the live result of every scoped mass cancel replays cleanly by
    /// identity, and the reconstructed book passes `snapshots_match`.
    #[test]
    fn test_replay_mass_cancel_identity_matches_live_results() {
        let (live, journal) = three_bids_fixture("LIVEID");
        let by_range = live.cancel_orders_by_price_range(Side::Buy, 101, 101);
        assert_eq!(by_range.cancelled_order_ids(), &[Id::from_u64(3)]);
        append_mass_cancel(
            &journal,
            3,
            SequencerCommand::CancelByPriceRange {
                side: Side::Buy,
                min_price: 101,
                max_price: 101,
            },
            by_range,
        );
        let by_user = live.cancel_orders_by_user(Hash32::zero());
        assert_eq!(by_user.cancelled_count(), 2);
        append_mass_cancel(
            &journal,
            4,
            SequencerCommand::CancelByUser {
                user_id: Hash32::zero(),
            },
            by_user,
        );
        assert_replay_matches(&live, &journal, "LIVEID");
    }

    /// #252: a level fault reported after the removal is not a book
    /// outcome: the order was cancelled live and on replay, so the fault
    /// report alone does not make the replay diverge.
    #[test]
    fn test_replay_mass_cancel_ignores_level_fault_reports() {
        let (live, journal) = level_fault_fixture("LFMASS");
        let result = live.cancel_orders_by_side(Side::Buy);
        assert!(result.has_failures() && !result.is_refused());
        assert_eq!(result.cancelled_count(), 2, "both removed");
        append_mass_cancel(
            &journal,
            2,
            SequencerCommand::CancelBySide { side: Side::Buy },
            result,
        );
        assert_replay_matches(&live, &journal, "LFMASS");
    }

    /// #252: failure outcomes are compared too. A replay book whose level
    /// refuses the same order the live one refused reproduces the partial
    /// result exactly and is accepted; one that refuses a different order
    /// diverges on the failed ids.
    #[test]
    fn test_replay_mass_cancel_compares_failure_outcomes() {
        use crate::orderbook::book::CancelFault;
        use crate::orderbook::mass_cancel::MassCancelFailure;

        let refuse = |order: u64| {
            move |id: Id| {
                (id == Id::from_u64(order)).then(|| {
                    CancelFault::Refuse(pricelevel::PriceLevelError::InvalidOperation {
                        message: "refused".to_string(),
                    })
                })
            }
        };
        let (_live, journal) = three_bids_fixture("FAILID");
        let recorded = MassCancelResult::with_failures(
            vec![Id::from_u64(1), Id::from_u64(3)],
            vec![MassCancelFailure::OrderCancelFailed {
                order_id: Id::from_u64(2),
                error: pricelevel::PriceLevelError::InvalidOperation {
                    message: "refused".to_string(),
                },
            }],
        );
        let event = SequencerEvent::<()> {
            sequence_num: 3,
            timestamp_ns: 0,
            command: SequencerCommand::CancelBySide { side: Side::Buy },
            result: SequencerResult::MassCancelled { result: recorded },
        };

        let rebuild = |hook_order: u64| {
            let mut book = OrderBook::<()>::new("FAILID");
            book.cancel_fault_hook = Some(Arc::new(refuse(hook_order)));
            for entry in journal.read_from(0).expect("read") {
                let ev = entry.expect("entry").event;
                ReplayEngine::<()>::apply_event(&book, &ev).expect("add replays");
            }
            book
        };

        // Same refused order: the partial result reproduces exactly.
        let book = rebuild(2);
        assert!(ReplayEngine::<()>::apply_event(&book, &event).is_ok());
        assert!(book.get_order(Id::from_u64(2)).is_some(), "still resting");

        // A different refused order: the ids diverge first.
        let book = rebuild(3);
        match ReplayEngine::<()>::apply_event(&book, &event) {
            Err(ReplayError::MassCancelMismatch { divergence, .. }) => assert_eq!(
                divergence,
                MassCancelDivergence::CancelledIds {
                    position: 1,
                    recorded_count: 2,
                    replayed_count: 2,
                }
            ),
            other => panic!("expected MassCancelMismatch, got {other:?}"),
        }
    }

    /// #252: identical cancelled ids but a recorded order left resting that
    /// the replay does not leave resting diverge on the failed ids.
    #[test]
    fn test_replay_mass_cancel_failed_ids_divergence() {
        use crate::orderbook::mass_cancel::MassCancelFailure;

        let (_live, journal) = three_bids_fixture("FAILONLY");
        append_mass_cancel(
            &journal,
            3,
            SequencerCommand::CancelBySide { side: Side::Buy },
            MassCancelResult::with_failures(
                vec![Id::from_u64(1), Id::from_u64(2), Id::from_u64(3)],
                vec![MassCancelFailure::OrderCancelFailed {
                    order_id: Id::from_u64(99),
                    error: pricelevel::PriceLevelError::InvalidOperation {
                        message: "refused".to_string(),
                    },
                }],
            ),
        );
        match ReplayEngine::<()>::replay_from(&journal, 0, "FAILONLY") {
            Err(ReplayError::MassCancelMismatch { divergence, .. }) => assert_eq!(
                divergence,
                MassCancelDivergence::FailedIds {
                    position: 0,
                    recorded_count: 1,
                    replayed_count: 0,
                }
            ),
            Err(other) => panic!("expected MassCancelMismatch, got {other:?}"),
            Ok(_) => panic!("a failure-outcome mismatch must not replay silently"),
        }
    }

    /// #252 review: a fill must not reorder a user's resting orders.
    /// `untrack_order_by_id` used `swap_remove`, so filling the user's first
    /// order moved their last one to the front and `cancel_orders_by_user`
    /// no longer followed admission order. The live result is now in
    /// admission order and replay reconciles it by identity.
    #[test]
    fn test_fill_keeps_user_order_admission_order_for_by_user_cancel() {
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        let live = OrderBook::<()>::new("FILLUSER");
        let bids = [(1u64, 100u128), (2, 99), (3, 98), (4, 97)];
        for (seq, (id, price)) in bids.into_iter().enumerate() {
            let ev = make_add_event(
                u64::try_from(seq).expect("seq"),
                Id::from_u64(id),
                price,
                5,
                Side::Buy,
            );
            if let SequencerCommand::AddOrder(order) = &ev.command {
                live.add_order(*order).expect("live add");
            }
            assert!(journal.append(&ev).is_ok());
        }

        // Fill the user's first (best) bid completely.
        let taker = Id::from_u64(50);
        let fill = live
            .submit_market_order(taker, 5, Side::Sell)
            .expect("live fill");
        assert!(
            live.get_order(Id::from_u64(1)).is_none(),
            "first bid filled"
        );
        assert!(
            journal
                .append(&SequencerEvent::<()> {
                    sequence_num: 4,
                    timestamp_ns: 0,
                    command: SequencerCommand::MarketOrder {
                        id: taker,
                        quantity: 5,
                        side: Side::Sell,
                    },
                    result: SequencerResult::TradeExecuted {
                        trade_result: TradeResult::new("FILLUSER".to_string(), fill)
                            .expect("trade result"),
                    },
                })
                .is_ok()
        );

        let by_user = live.cancel_orders_by_user(Hash32::zero());
        assert_eq!(
            by_user.cancelled_order_ids(),
            &[Id::from_u64(2), Id::from_u64(3), Id::from_u64(4)],
            "admission order survives the fill"
        );
        append_mass_cancel(
            &journal,
            5,
            SequencerCommand::CancelByUser {
                user_id: Hash32::zero(),
            },
            by_user,
        );
        assert_replay_matches(&live, &journal, "FILLUSER");
    }

    #[test]
    fn test_first_difference_positions() {
        assert_eq!(first_difference::<u8>(&[], &[]), None);
        assert_eq!(first_difference(&[1, 2], &[1, 2]), None);
        assert_eq!(first_difference(&[1, 2], &[1, 3]), Some(1));
        assert_eq!(first_difference(&[1, 2], &[1]), Some(1));
        assert_eq!(first_difference(&[1], &[1, 2]), Some(1));
        assert_eq!(first_difference(&[], &[1]), Some(0));
    }

    // --- trade-ID namespace through replay (#200) ---------------------------

    /// Seeds a resting sell then sweeps it with a market buy, returning the
    /// emitted trade ID. Trade IDs are UUID v5 of (namespace, counter), so an
    /// equal probe ID across two books proves both the namespace and the
    /// counter position are equal — which in turn proves every earlier trade
    /// ID the two books emitted was identical.
    fn probe_next_trade_id(book: &OrderBook<()>) -> String {
        let resting = new_id();
        book.add_limit_order(resting, 1_000, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("probe resting bid");
        let taker = new_id();
        let result = book
            .match_market_order(taker, 10, Side::Sell)
            .expect("probe market sell");
        let trades = result.trades();
        let tx = trades.as_vec().first().cloned().expect("probe trade");
        tx.trade_id().to_string()
    }

    /// Journal with one resting sell and a market buy that trades against it,
    /// so a replay advances the reconstructed book's trade-ID counter.
    fn trading_journal(maker_id: Id, taker_id: Id) -> InMemoryJournal<()> {
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        assert!(
            journal
                .append(&make_add_event(0, maker_id, 100, 10, Side::Sell))
                .is_ok()
        );
        let ev = SequencerEvent::<()> {
            sequence_num: 1,
            timestamp_ns: 0,
            command: SequencerCommand::MarketOrder {
                id: taker_id,
                quantity: 10,
                side: Side::Buy,
            },
            result: SequencerResult::TradeExecuted {
                trade_result: TradeResult::new(
                    "TEST".to_string(),
                    MatchResult::new(taker_id, Quantity::new(0)),
                )
                .expect("valid trade result"),
            },
        };
        assert!(journal.append(&ev).is_ok());
        journal
    }

    /// #200: `ReplayBookConfig::default` leaves the namespace unset and the
    /// builder sets it without disturbing the structural fields.
    #[test]
    fn test_replay_book_config_trade_id_namespace_builder_and_default() {
        assert_eq!(ReplayBookConfig::default().trade_id_namespace, None);

        let ns = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"VENUE/TEST");
        let fee = FeeSchedule::new(-2, 5);
        let config = ReplayBookConfig::new(Some(fee), STPMode::None, Some(10), None, None, None)
            .with_trade_id_namespace(ns);
        assert_eq!(config.trade_id_namespace, Some(ns));
        assert_eq!(config.fee_schedule, Some(fee));
        assert_eq!(config.tick_size, Some(10));
    }

    /// #200: a namespace-carrying config makes the replayed trade-ID stream
    /// byte-identical to a reference book that used the same namespace and
    /// command stream (probe equality — see `probe_next_trade_id`).
    #[test]
    fn test_replay_with_config_namespace_reproduces_trade_id_stream() {
        let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"VENUE/TEST");
        let maker_id = new_id();
        let taker_id = new_id();
        let journal = trading_journal(maker_id, taker_id);

        // Reference "live" book: same namespace, same command stream.
        let reference = OrderBook::<()>::with_clock_and_namespace(
            "TEST",
            Arc::new(StubClock::new()) as Arc<dyn Clock>,
            namespace,
        );
        reference
            .add_limit_order(maker_id, 100, 10, Side::Sell, TimeInForce::Gtc, None)
            .expect("reference maker");
        let live_trades = reference
            .submit_market_order(taker_id, 10, Side::Buy)
            .expect("reference taker");
        assert!(
            !live_trades.trades().as_vec().is_empty(),
            "reference stream must trade"
        );

        let config = ReplayBookConfig::default().with_trade_id_namespace(namespace);
        let clock: Arc<dyn Clock> = Arc::new(StubClock::new());
        let (replayed, _) = ReplayEngine::<()>::replay_from_with_clock_and_config(
            &journal, 0, "TEST", clock, &config,
        )
        .expect("replay with namespace config");

        assert_eq!(
            probe_next_trade_id(&reference),
            probe_next_trade_id(&replayed),
            "equal probe IDs prove namespace + counter position match, hence \
             the whole replayed trade-ID stream matched the reference"
        );
    }

    /// A namespace-carrying config builds the replayed book with the pure
    /// constructor: it never derives the default namespace, so the replay
    /// reads no wall clock, pid or construction counter. Without one, the
    /// replay derives the default namespace once, as before.
    #[test]
    fn test_replay_with_config_namespace_construction_skips_default_namespace() {
        let derivations =
            || crate::orderbook::book::DEFAULT_NAMESPACE_DERIVATIONS.with(std::cell::Cell::get);
        let journal = trading_journal(new_id(), new_id());
        let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"VENUE/TEST");
        let with_namespace = ReplayBookConfig::default().with_trade_id_namespace(namespace);

        let before = derivations();
        let clock: Arc<dyn Clock> = Arc::new(StubClock::new());
        let _ = ReplayEngine::<()>::replay_from_with_clock_and_config(
            &journal,
            0,
            "TEST",
            clock,
            &with_namespace,
        )
        .expect("replay with namespace config");
        assert_eq!(derivations(), before);

        let clock: Arc<dyn Clock> = Arc::new(StubClock::new());
        let _ = ReplayEngine::<()>::replay_from_with_clock_and_config(
            &journal,
            0,
            "TEST",
            clock,
            &ReplayBookConfig::default(),
        )
        .expect("replay with default config");
        assert_eq!(derivations(), before + 1);
    }

    /// #200 review: a namespace-carrying config on a suffix replay
    /// (`from_sequence != 0`) must be rejected — applying the namespace
    /// restarts the trade-ID counter at 0, so a suffix would mint wrong IDs
    /// and duplicates of IDs already emitted live under that namespace.
    #[test]
    fn test_replay_with_namespace_config_rejects_suffix_replay() {
        let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"VENUE/TEST");
        let journal = trading_journal(new_id(), new_id());
        let config = ReplayBookConfig::default().with_trade_id_namespace(namespace);

        // from_sequence = 1 is a valid suffix of the two-event journal, so
        // the InvalidSequence pre-check passes and the namespace guard must
        // be the one that fires — on both config entry points.
        match ReplayEngine::<()>::replay_from_with_config(&journal, 1, "TEST", &config) {
            Err(ReplayError::NamespaceRequiresFullReplay { from_sequence }) => {
                assert_eq!(from_sequence, 1);
            }
            Err(other) => panic!("expected NamespaceRequiresFullReplay, got {other:?}"),
            Ok(_) => panic!("suffix replay with a namespace must be rejected"),
        }

        let clock: Arc<dyn Clock> = Arc::new(StubClock::new());
        match ReplayEngine::<()>::replay_from_with_clock_and_config(
            &journal, 1, "TEST", clock, &config,
        ) {
            Err(ReplayError::NamespaceRequiresFullReplay { from_sequence }) => {
                assert_eq!(from_sequence, 1);
            }
            Err(other) => panic!("expected NamespaceRequiresFullReplay, got {other:?}"),
            Ok(_) => panic!("suffix replay with a namespace must be rejected"),
        }
    }

    /// #200 review: the suffix-replay guard only bites when a namespace is
    /// present — a namespace-free config still supports suffix replay, and
    /// an out-of-range from_sequence keeps its InvalidSequence precedence
    /// even with a namespace.
    #[test]
    fn test_replay_suffix_without_namespace_still_allowed() {
        // Two independent adds so the seq-1 suffix is self-contained.
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        assert!(
            journal
                .append(&make_add_event(0, new_id(), 100, 10, Side::Buy))
                .is_ok()
        );
        assert!(
            journal
                .append(&make_add_event(1, new_id(), 99, 10, Side::Buy))
                .is_ok()
        );

        // Suffix replay with a namespace-free config: the pre-#200 behavior.
        let result = ReplayEngine::<()>::replay_from_with_config(
            &journal,
            1,
            "TEST",
            &ReplayBookConfig::default(),
        );
        match result {
            Ok((_, last_seq)) => assert_eq!(last_seq, 1),
            Err(err) => panic!("namespace-free suffix replay must keep working, got {err:?}"),
        }

        // Out-of-range from_sequence: InvalidSequence fires before the
        // namespace guard.
        let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"VENUE/TEST");
        let config = ReplayBookConfig::default().with_trade_id_namespace(namespace);
        match ReplayEngine::<()>::replay_from_with_config(&journal, 5, "TEST", &config) {
            Err(ReplayError::InvalidSequence { from_sequence, .. }) => {
                assert_eq!(from_sequence, 5);
            }
            Err(other) => panic!("expected InvalidSequence, got {other:?}"),
            Ok(_) => panic!("expected InvalidSequence, got Ok(_)"),
        }
    }

    /// #200: without a namespace in the config, the fresh book keeps its
    /// random namespace — two replays of the same journal diverge.
    #[test]
    fn test_replay_with_default_config_keeps_random_namespace() {
        let journal = trading_journal(new_id(), new_id());

        let (a, _) = ReplayEngine::<()>::replay_from_with_config(
            &journal,
            0,
            "TEST",
            &ReplayBookConfig::default(),
        )
        .expect("first default-config replay");
        let (b, _) = ReplayEngine::<()>::replay_from_with_config(
            &journal,
            0,
            "TEST",
            &ReplayBookConfig::default(),
        )
        .expect("second default-config replay");

        assert_ne!(
            probe_next_trade_id(&a),
            probe_next_trade_id(&b),
            "default config must keep per-replay random namespaces"
        );
    }

    // --- #240: aborted sweeps in the journal ---------------------------

    /// A trade-id generator that can still mint exactly `remaining` ids,
    /// restored through serde like a persisted generator state.
    fn generator_with_remaining(remaining: u64) -> pricelevel::UuidGenerator {
        let counter = u64::MAX.checked_sub(remaining).expect("remaining ids");
        let json = format!(
            r#"{{"namespace":"6ba7b810-9dad-11d1-80b4-00c04fd430c8","counter":{counter}}}"#
        );
        serde_json::from_str(&json).expect("restore generator")
    }

    fn book_with_remaining_ids(remaining: u64) -> OrderBook<()> {
        let mut book = OrderBook::<()>::new("TEST");
        book.transaction_id_generator = generator_with_remaining(remaining);
        book
    }

    fn standard(id: u64, price: u128, qty: u64, side: Side) -> OrderType<()> {
        OrderType::Standard {
            id: Id::from_u64(id),
            price: Price::new(price),
            quantity: Quantity::new(qty),
            side,
            time_in_force: TimeInForce::Gtc,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        }
    }

    /// Executes `commands` on `live`, journaling each outcome the way a
    /// sequencer is expected to: submits through `add_order_with_committed`
    /// and `SequencerResult::from_submit_failure`, updates through
    /// `From<&OrderBookError>`.
    fn run_live(live: &OrderBook<()>, commands: Vec<SequencerCommand<()>>) -> InMemoryJournal<()> {
        let journal: InMemoryJournal<()> = InMemoryJournal::new();
        for (seq, command) in commands.into_iter().enumerate() {
            let result = match &command {
                SequencerCommand::AddOrder(order) => match live.add_order_with_committed(*order) {
                    Ok((order, _)) => SequencerResult::OrderAdded {
                        order_id: order.id(),
                    },
                    Err(failure) => {
                        SequencerResult::from_submit_failure(&failure).expect("record failure")
                    }
                },
                SequencerCommand::UpdateOrder(update) => match live.update_order(*update) {
                    Ok(_) => SequencerResult::OrderUpdated {
                        order_id: match update {
                            pricelevel::OrderUpdate::UpdatePriceAndQuantity {
                                order_id, ..
                            } => *order_id,
                            other => panic!("unsupported update in fixture: {other:?}"),
                        },
                    },
                    Err(err) => SequencerResult::from(&err),
                },
                other => panic!("unsupported command in fixture: {other:?}"),
            };
            let event = SequencerEvent {
                sequence_num: u64::try_from(seq).expect("seq"),
                timestamp_ns: 0,
                command,
                result,
            };
            journal.append(&event).expect("append");
        }
        journal
    }

    /// Asks A 5@100, B 5@101, C 5@101, D 5@102, then a GTC buy of 20 @102.
    fn aborted_sweep_commands() -> Vec<SequencerCommand<()>> {
        vec![
            SequencerCommand::AddOrder(standard(1, 100, 5, Side::Sell)),
            SequencerCommand::AddOrder(standard(2, 101, 5, Side::Sell)),
            SequencerCommand::AddOrder(standard(3, 101, 5, Side::Sell)),
            SequencerCommand::AddOrder(standard(4, 102, 5, Side::Sell)),
            SequencerCommand::AddOrder(standard(100, 102, 20, Side::Buy)),
        ]
    }

    fn replay_onto(book: &OrderBook<()>, journal: &InMemoryJournal<()>) -> Result<(), ReplayError> {
        for entry in journal.read_from(0).expect("read") {
            let entry = entry.expect("entry");
            ReplayEngine::<()>::apply_event(book, &entry.event)?;
        }
        Ok(())
    }

    #[test]
    fn test_replay_match_aborted_journal_records_the_prefix() {
        let live = book_with_remaining_ids(2);
        let journal = run_live(&live, aborted_sweep_commands());
        let last = journal
            .read_from(4)
            .expect("read")
            .next()
            .expect("event")
            .expect("entry");
        match &last.event.result {
            SequencerResult::MatchAborted {
                code, committed, ..
            } => {
                assert_eq!(*code, RejectReason::MatchAborted);
                assert_eq!(committed.executed_quantity, 10);
                let makers: Vec<Id> = committed.trades.iter().map(|t| t.maker_order_id).collect();
                assert_eq!(makers, vec![Id::from_u64(1), Id::from_u64(2)]);
            }
            other => panic!("expected MatchAborted, got {other:?}"),
        }
    }

    #[test]
    fn test_replay_match_aborted_reproduces_the_committed_prefix() {
        let live = book_with_remaining_ids(2);
        let journal = run_live(&live, aborted_sweep_commands());

        // Same resource state as the live book: the abort reproduces.
        let replayed = book_with_remaining_ids(2);
        replay_onto(&replayed, &journal).expect("replay reproduces the abort");
        let expected = live.create_snapshot(usize::MAX).expect("live snapshot");
        let actual = replayed
            .create_snapshot(usize::MAX)
            .expect("replay snapshot");
        assert!(snapshots_match(&actual, &expected));
        assert_eq!(replayed.best_bid(), None, "the remainder never rests");
    }

    #[test]
    fn test_replay_match_aborted_detects_a_replay_that_fills_further() {
        let live = book_with_remaining_ids(2);
        let journal = run_live(&live, aborted_sweep_commands());

        // A fresh replay book has its whole id sequence: the taker fills
        // past the recorded prefix, which must not pass silently.
        match ReplayEngine::<()>::replay_from(&journal, 0, "TEST") {
            Err(ReplayError::OutcomeMismatch {
                sequence_num,
                recorded,
                actual,
            }) => {
                assert_eq!(sequence_num, 4);
                assert_eq!(recorded, RejectReason::MatchAborted);
                assert!(actual.is_none(), "replay succeeded: {actual:?}");
            }
            Err(other) => panic!("expected OutcomeMismatch, got {other:?}"),
            Ok(_) => panic!("expected OutcomeMismatch, replay succeeded"),
        }
    }

    #[test]
    fn test_replay_match_aborted_detects_a_different_prefix() {
        let live = book_with_remaining_ids(2);
        let journal = run_live(&live, aborted_sweep_commands());

        // One more id: the replay aborts too, but after three trades.
        let replayed = book_with_remaining_ids(3);
        match replay_onto(&replayed, &journal) {
            Err(ReplayError::OutcomeMismatch {
                sequence_num,
                recorded,
                actual: Some(OrderBookError::MatchAborted { trade_count, .. }),
            }) => {
                assert_eq!(sequence_num, 4);
                assert_eq!(recorded, RejectReason::MatchAborted);
                assert_eq!(trade_count, 3);
            }
            other => panic!("expected OutcomeMismatch on the prefix, got {other:?}"),
        }
    }

    #[test]
    fn test_replay_aborted_update_is_reexecuted_and_reconciled() {
        let commands = vec![
            SequencerCommand::AddOrder(standard(1, 100, 5, Side::Sell)),
            SequencerCommand::AddOrder(standard(2, 101, 5, Side::Sell)),
            SequencerCommand::AddOrder(standard(50, 99, 5, Side::Buy)),
            SequencerCommand::UpdateOrder(pricelevel::OrderUpdate::UpdatePriceAndQuantity {
                order_id: Id::from_u64(50),
                new_price: Price::new(101),
                new_quantity: Quantity::new(10),
            }),
        ];
        let live = book_with_remaining_ids(1);
        let journal = run_live(&live, commands);
        let last = journal
            .read_from(3)
            .expect("read")
            .next()
            .expect("event")
            .expect("entry");
        assert!(
            matches!(
                last.event.result,
                SequencerResult::RejectedWithCode {
                    code: RejectReason::MatchAborted,
                    may_have_mutated: true,
                    ..
                }
            ),
            "the re-add of the update aborted: {:?}",
            last.event.result
        );

        // Reproduced: same book.
        let replayed = book_with_remaining_ids(1);
        replay_onto(&replayed, &journal).expect("aborted update reproduces");
        let expected = live.create_snapshot(usize::MAX).expect("live snapshot");
        let actual = replayed
            .create_snapshot(usize::MAX)
            .expect("replay snapshot");
        assert!(snapshots_match(&actual, &expected));

        // Not reproduced: loud, never skipped.
        match ReplayEngine::<()>::replay_from(&journal, 0, "TEST") {
            Err(ReplayError::OutcomeMismatch {
                sequence_num: 3,
                recorded: RejectReason::MatchAborted,
                actual: None,
            }) => {}
            Err(other) => panic!("expected OutcomeMismatch, got {other:?}"),
            Ok(_) => panic!("expected OutcomeMismatch, replay succeeded"),
        }
    }
}
