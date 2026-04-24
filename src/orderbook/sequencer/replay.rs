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
use crate::orderbook::{OrderBook, OrderBookError, OrderBookSnapshot};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::sync::Arc;
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
    /// For deterministic replay with a custom clock, see
    /// [`Self::replay_from_with_clock`].
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

            Self::apply_event(book, event)?;
            last_applied_seq = event.sequence_num;
            count = count.saturating_add(1);
            expected_seq = expected_seq.saturating_add(1);
            progress(count, last_applied_seq);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orderbook::clock::{MonotonicClock, StubClock};
    use crate::orderbook::sequencer::InMemoryJournal;
    use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};

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
}
