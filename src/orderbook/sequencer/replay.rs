//! Deterministic replay engine for event journals.
//!
//! [`ReplayEngine`] reads a sequence of [`SequencerEvent`]s from a [`Journal`]
//! and re-applies each command to a fresh [`OrderBook`], producing an
//! identical final state. This enables disaster recovery, audit compliance,
//! and state verification.

use super::error::JournalError;
use super::journal::Journal;
use super::types::{SequencerCommand, SequencerEvent, SequencerResult};
use crate::orderbook::{OrderBook, OrderBookError, OrderBookSnapshot};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use thiserror::Error;

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

    /// An OrderBook operation failed during replay.
    #[error("order book error during replay at sequence {sequence_num}: {source}")]
    OrderBookError {
        /// The sequence number of the event that caused the error.
        sequence_num: u64,
        /// The underlying error.
        #[source]
        source: OrderBookError,
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

            Self::apply_event(&book, event)?;
            last_applied_seq = event.sequence_num;
            count = count.saturating_add(1);
            expected_seq = expected_seq.saturating_add(1);
            progress(count, last_applied_seq);
        }

        Ok((book, last_applied_seq))
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
        }

        Ok(())
    }
}

/// Compares two [`OrderBookSnapshot`]s for structural equality.
///
/// Two snapshots are considered equal when:
/// - `symbol` is identical
/// - The sorted bid price levels match (by price, then visible quantity)
/// - The sorted ask price levels match (by price, then visible quantity)
///
/// Timestamps are intentionally excluded from comparison because replayed
/// books may be created at a different wall-clock time than the original.
#[must_use]
pub fn snapshots_match(actual: &OrderBookSnapshot, expected: &OrderBookSnapshot) -> bool {
    if actual.symbol != expected.symbol {
        return false;
    }

    // Compare bids sorted by price descending (highest bid first)
    let mut actual_bids: Vec<_> = actual.bids.iter().collect();
    let mut expected_bids: Vec<_> = expected.bids.iter().collect();
    actual_bids.sort_by_key(|b| std::cmp::Reverse(b.price()));
    expected_bids.sort_by_key(|b| std::cmp::Reverse(b.price()));

    if actual_bids.len() != expected_bids.len() {
        return false;
    }
    for (a, b) in actual_bids.iter().zip(expected_bids.iter()) {
        if a.price() != b.price() || a.visible_quantity() != b.visible_quantity() {
            return false;
        }
    }

    // Compare asks sorted by price ascending (lowest ask first)
    let mut actual_asks: Vec<_> = actual.asks.iter().collect();
    let mut expected_asks: Vec<_> = expected.asks.iter().collect();
    actual_asks.sort_by_key(|l| l.price());
    expected_asks.sort_by_key(|l| l.price());

    if actual_asks.len() != expected_asks.len() {
        return false;
    }
    for (a, b) in actual_asks.iter().zip(expected_asks.iter()) {
        if a.price() != b.price() || a.visible_quantity() != b.visible_quantity() {
            return false;
        }
    }

    true
}
