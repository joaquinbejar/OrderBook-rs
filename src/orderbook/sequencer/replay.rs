//! Deterministic replay engine for event journals.
//!
//! [`ReplayEngine`] reads a sequence of [`SequencerEvent`]s from a [`Journal`]
//! and re-applies each command to a fresh [`OrderBook`], producing an
//! identical final state. This enables disaster recovery, audit compliance,
//! and state verification.

use super::error::JournalError;
use super::journal::Journal;
use super::types::{SequencerCommand, SequencerEvent, SequencerResult};
use crate::orderbook::clock::Clock;
use crate::orderbook::fees::FeeSchedule;
use crate::orderbook::stp::STPMode;
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
    /// [`OrderBook::set_trade_id_namespace`].
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
    }
}

/// Errors that can occur during journal replay.
#[derive(Debug, Error)]
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

    /// The replayed state does not match the expected snapshot.
    #[error("snapshot mismatch: replayed state diverges from expected snapshot")]
    SnapshotMismatch,

    /// Journal read error during replay.
    #[error("journal error during replay: {0}")]
    JournalError(#[from] JournalError),
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
    /// event applied. Only successful commands (non-`Rejected` results) are
    /// replayed — rejected events are skipped without error.
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
    /// - [`ReplayError::JournalError`] if reading from the journal fails
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
    /// Useful for long replays where progress reporting is needed.
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
        let last_seq = match journal.last_sequence() {
            Some(seq) => seq,
            None => return Err(ReplayError::EmptyJournal),
        };

        if from_sequence > last_seq {
            return Err(ReplayError::InvalidSequence {
                from_sequence,
                last_sequence: last_seq,
            });
        }

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
        let last_seq = match journal.last_sequence() {
            Some(seq) => seq,
            None => return Err(ReplayError::EmptyJournal),
        };

        if from_sequence > last_seq {
            return Err(ReplayError::InvalidSequence {
                from_sequence,
                last_sequence: last_seq,
            });
        }

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
    /// successfully-applied event to a progress callback. Useful for long
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
        let last_seq = match journal.last_sequence() {
            Some(seq) => seq,
            None => return Err(ReplayError::EmptyJournal),
        };

        if from_sequence > last_seq {
            return Err(ReplayError::InvalidSequence {
                from_sequence,
                last_sequence: last_seq,
            });
        }

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
        let last_seq = match journal.last_sequence() {
            Some(seq) => seq,
            None => return Err(ReplayError::EmptyJournal),
        };

        if from_sequence > last_seq {
            return Err(ReplayError::InvalidSequence {
                from_sequence,
                last_sequence: last_seq,
            });
        }

        Self::check_namespace_full_replay(from_sequence, config)?;

        let mut book = OrderBook::with_clock(symbol, clock);
        config.apply_to(&mut book);
        let last_applied_seq = Self::replay_into(&book, journal, from_sequence, |_, _| {})?;
        Ok((book, last_applied_seq))
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
            // correct even if the event is a rejected no-op. `last_applied_seq`,
            // `count`, and `progress` track only events that actually mutate
            // the book — consistent with the "events applied" / "last applied
            // sequence" contract on the public entry points.
            let applied = !matches!(event.result, SequencerResult::Rejected { .. });
            Self::apply_event(book, event)?;
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

        Ok(last_applied_seq)
    }

    /// Replays the full journal and compares the result to an expected snapshot.
    ///
    /// Returns `Ok(true)` if the replayed state matches, `Ok(false)` if it
    /// diverges. The comparison uses [`snapshots_match`] which checks symbol,
    /// bid price levels, and ask price levels.
    ///
    /// # Errors
    ///
    /// - [`ReplayError::EmptyJournal`] if the journal has no events
    /// - [`ReplayError::OrderBookError`] if replay fails
    /// - [`ReplayError::JournalError`] if reading from the journal fails
    pub fn verify(
        journal: &impl Journal<T>,
        expected_snapshot: &OrderBookSnapshot,
    ) -> Result<bool, ReplayError> {
        let (book, _) = Self::replay_from(journal, 0, &expected_snapshot.symbol)?;
        let actual = book.create_snapshot(usize::MAX);
        Ok(snapshots_match(&actual, expected_snapshot))
    }

    /// Applies a single sequencer event to the given book.
    ///
    /// Events with `Rejected` results are skipped — they represent commands
    /// that failed at write time and must not be re-applied during replay.
    fn apply_event(book: &OrderBook<T>, event: &SequencerEvent<T>) -> Result<(), ReplayError> {
        // Skip events whose original execution was rejected.
        if matches!(event.result, SequencerResult::Rejected { .. }) {
            return Ok(());
        }

        match &event.command {
            SequencerCommand::AddOrder(order) => {
                book.add_order(order.clone())
                    .map_err(|e| ReplayError::OrderBookError {
                        sequence_num: event.sequence_num,
                        source: e,
                    })?;
            }
            SequencerCommand::CancelOrder(id) => {
                book.cancel_order(*id)
                    .map_err(|e| ReplayError::OrderBookError {
                        sequence_num: event.sequence_num,
                        source: e,
                    })?;
            }
            SequencerCommand::UpdateOrder(update) => {
                book.update_order(*update)
                    .map_err(|e| ReplayError::OrderBookError {
                        sequence_num: event.sequence_num,
                        source: e,
                    })?;
            }
            SequencerCommand::MarketOrder { id, quantity, side } => {
                book.submit_market_order(*id, *quantity, *side)
                    .map_err(|e| ReplayError::OrderBookError {
                        sequence_num: event.sequence_num,
                        source: e,
                    })?;
            }
            SequencerCommand::MarketOrderByAmount { id, amount, side } => {
                book.submit_market_order_by_amount(*id, *amount, *side)
                    .map_err(|e| ReplayError::OrderBookError {
                        sequence_num: event.sequence_num,
                        source: e,
                    })?;
            }
            SequencerCommand::CancelAll => {
                let _ = book.cancel_all_orders();
            }
            SequencerCommand::CancelBySide { side } => {
                let _ = book.cancel_orders_by_side(*side);
            }
            SequencerCommand::CancelByUser { user_id } => {
                let _ = book.cancel_orders_by_user(*user_id);
            }
            SequencerCommand::CancelByPriceRange {
                side,
                min_price,
                max_price,
            } => {
                let _ = book.cancel_orders_by_price_range(*side, *min_price, *max_price);
            }
            SequencerCommand::EvictExpiredOrders { now_ms } => {
                // Apply the journaled cutoff, never the replay clock, so the
                // sweep evicts exactly the orders it evicted live. The sweep
                // is idempotent, so a duplicate replay is a no-op.
                let _ = book.evict_expired_orders(*now_ms);
            }
        }

        Ok(())
    }
}

/// Compares two [`OrderBookSnapshot`]s for structural equality.
///
/// Two snapshots are considered equal when:
/// - `symbol` is identical
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
/// materialized in queue-consumption order, so element-wise [`OrderType`]
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
/// - `first_arrival_time` — pricelevel derives it from a raw
///   `SystemTime::now()` at level creation, outside the injectable `Clock`,
///   so it can never match between two runs;
/// - `last_execution_time` / `sum_waiting_time` — clock-derived, but live
///   ingestion and replay consume different clock-tick budgets by design
///   (the live submission API stamps each order with a fresh tick; replay
///   reuses the journal's pre-stamped order), so these wall-time aggregates
///   diverge even under identically-seeded injected clocks;
/// - the top-level snapshot capture timestamp, as before.
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

    sides_match(&actual.bids, &expected.bids) && sides_match(&actual.asks, &expected.asks)
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
        level.snapshot()
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
        };
        let degraded = OrderBookSnapshot {
            symbol: "STATS".to_string(),
            timestamp: 7,
            bids: vec![lvl_with_stats(true, 1_000)],
            asks: Vec::new(),
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
        let ev = make_add_event(u64::MAX, Id::new_uuid(), 100, 10, Side::Buy);
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
            let ev = make_add_event(seq, Id::new_uuid(), price, 10, Side::Buy);
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
        let ids: Vec<Id> = (0..3).map(|_| Id::new_uuid()).collect();
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

        let snap_plain = book_plain.create_snapshot(usize::MAX);
        let snap_with_clock = book_with_clock.create_snapshot(usize::MAX);
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
            make_add_event(0, Id::new_uuid(), 100, 1, Side::Buy),
            make_add_event(1, Id::new_uuid(), 101, 1, Side::Buy),
            make_add_event(2, Id::new_uuid(), 102, 1, Side::Buy),
            make_add_event(4, Id::new_uuid(), 104, 1, Side::Buy),
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
        let taker_id = Id::new_uuid();
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
                ),
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

        let live_snap = live_book.create_snapshot(usize::MAX);
        let replayed_snap = replayed.create_snapshot(usize::MAX);
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
        let ev = make_add_event(0, Id::new_uuid(), 100, 10, Side::Buy);
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
        let ev = make_add_event(0, Id::new_uuid(), 100, 10, Side::Buy);
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
            id: Id::new_uuid(),
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
            id: Id::new_uuid(),
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
        use crate::orderbook::mass_cancel::MassCancelResult;

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
        let expiring_bid = order(Id::new_uuid(), 100, 5, Side::Buy, TimeInForce::Gtd(1_000));
        let future_bid = order(Id::new_uuid(), 99, 3, Side::Buy, TimeInForce::Gtd(10_000));
        let gtc_bid = order(Id::new_uuid(), 98, 4, Side::Buy, TimeInForce::Gtc);
        let expiring_ask = order(Id::new_uuid(), 101, 7, Side::Sell, TimeInForce::Gtd(1_000));
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
        let evicted = live.evict_expired_orders(now);
        assert_eq!(evicted.len(), 2, "two GTD orders expire by t=5_000");
        let sweep = SequencerEvent::<()> {
            sequence_num: seq,
            timestamp_ns: 0,
            command: SequencerCommand::EvictExpiredOrders { now_ms: now },
            result: SequencerResult::MassCancelled {
                result: MassCancelResult::new(
                    evicted.len(),
                    evicted.iter().map(|o| o.id()).collect(),
                ),
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

        let live_snap = live.create_snapshot(usize::MAX);
        let replayed_snap = replayed.create_snapshot(usize::MAX);
        assert!(
            snapshots_match(&live_snap, &replayed_snap),
            "post-sweep live and replayed snapshots must match"
        );

        // Sanity: the expired levels are gone, the survivors remain.
        assert_eq!(replayed_snap.bids.len(), 2, "99 and 98 bids survive");
        assert!(replayed_snap.asks.is_empty(), "the only ask expired");
    }

    // --- trade-ID namespace through replay (#200) ---------------------------

    /// Seeds a resting sell then sweeps it with a market buy, returning the
    /// emitted trade ID. Trade IDs are UUID v5 of (namespace, counter), so an
    /// equal probe ID across two books proves both the namespace and the
    /// counter position are equal — which in turn proves every earlier trade
    /// ID the two books emitted was identical.
    fn probe_next_trade_id(book: &OrderBook<()>) -> String {
        let resting = Id::new_uuid();
        book.add_limit_order(resting, 1_000, 10, Side::Buy, TimeInForce::Gtc, None)
            .expect("probe resting bid");
        let taker = Id::new_uuid();
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
                ),
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
        let maker_id = Id::new_uuid();
        let taker_id = Id::new_uuid();
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

    /// #200 review: a namespace-carrying config on a suffix replay
    /// (`from_sequence != 0`) must be rejected — applying the namespace
    /// restarts the trade-ID counter at 0, so a suffix would mint wrong IDs
    /// and duplicates of IDs already emitted live under that namespace.
    #[test]
    fn test_replay_with_namespace_config_rejects_suffix_replay() {
        let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"VENUE/TEST");
        let journal = trading_journal(Id::new_uuid(), Id::new_uuid());
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
                .append(&make_add_event(0, Id::new_uuid(), 100, 10, Side::Buy))
                .is_ok()
        );
        assert!(
            journal
                .append(&make_add_event(1, Id::new_uuid(), 99, 10, Side::Buy))
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
        let journal = trading_journal(Id::new_uuid(), Id::new_uuid());

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
}
