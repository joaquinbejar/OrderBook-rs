//! In-memory journal implementation for testing and benchmarking.
//!
//! [`InMemoryJournal`] stores all events in a `Vec` in insertion order.
//! Suitable for testing, benchmarking, and short-lived workloads where
//! persistence is not required.

use super::error::JournalError;
use super::journal::{Journal, JournalEntry, JournalReadIter};
use super::types::SequencerEvent;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;

/// In-memory implementation of [`Journal`].
///
/// Stores all events in a `Vec` in insertion order. Suitable for testing,
/// benchmarking, and short-lived workloads where persistence is not required.
///
/// # Examples
///
/// ```
/// use orderbook_rs::orderbook::sequencer::{InMemoryJournal, Journal, SequencerCommand, SequencerEvent, SequencerResult};
/// use pricelevel::Id;
///
/// let journal: InMemoryJournal<()> = InMemoryJournal::new();
/// assert_eq!(journal.last_sequence(), None);
///
/// let event = SequencerEvent {
///     sequence_num: 1,
///     timestamp_ns: 0,
///     command: SequencerCommand::CancelOrder(Id::new()),
///     result: SequencerResult::OrderCancelled { order_id: Id::new() },
/// };
/// journal.append(&event).ok();
/// assert_eq!(journal.last_sequence(), Some(1));
/// ```
#[derive(Debug)]
pub struct InMemoryJournal<T> {
    events: RwLock<Vec<SequencerEvent<T>>>,
}

impl<T> Default for InMemoryJournal<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> InMemoryJournal<T> {
    /// Creates a new empty in-memory journal.
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
        }
    }

    /// Creates a new in-memory journal with pre-allocated capacity.
    ///
    /// Use this when the approximate number of events is known in advance
    /// to avoid repeated reallocations.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            events: RwLock::new(Vec::with_capacity(capacity)),
        }
    }

    /// Returns the total number of events stored.
    ///
    /// Returns 0 if the lock is poisoned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.read().map(|e| e.len()).unwrap_or(0)
    }

    /// Returns `true` if no events have been appended.
    ///
    /// Returns `true` if the lock is poisoned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.read().map(|e| e.is_empty()).unwrap_or(true)
    }
}

impl<T> Journal<T> for InMemoryJournal<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + 'static,
{
    fn append(&self, event: &SequencerEvent<T>) -> Result<(), JournalError> {
        self.events
            .write()
            .map_err(|_| JournalError::Io {
                message: "failed to acquire write lock".to_string(),
                path: None,
            })?
            .push(event.clone());
        Ok(())
    }

    fn read_from(&self, sequence: u64) -> Result<JournalReadIter<T>, JournalError> {
        let events = self.events.read().map_err(|_| JournalError::Io {
            message: "failed to acquire read lock".to_string(),
            path: None,
        })?;

        let filtered: Vec<_> = events
            .iter()
            .filter(|e| e.sequence_num >= sequence)
            .map(|event| {
                Ok(JournalEntry {
                    event: event.clone(),
                    stored_crc: 0, // No CRC for in-memory journal
                })
            })
            .collect();

        Ok(Box::new(filtered.into_iter()))
    }

    fn last_sequence(&self) -> Option<u64> {
        self.events.read().ok()?.last().map(|e| e.sequence_num)
    }

    fn verify_integrity(&self) -> Result<(), JournalError> {
        // In-memory journal has no on-disk representation, so integrity is always valid
        Ok(())
    }
}
