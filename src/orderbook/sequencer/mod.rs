//! Sequencer subsystem for total-ordered event processing and journaling.
//!
//! This module provides the core types and traits for the single-threaded
//! Sequencer (LMAX Disruptor pattern) and its append-only event journal.
//!
//! # Types
//!
//! - [`SequencerCommand`] — commands submitted for sequenced execution
//! - [`SequencerEvent`] — sequenced events emitted after execution
//! - [`SequencerResult`] — outcomes of command execution
//! - [`JournalError`] — error type for journal operations
//! - [`Journal`] — trait for append-only event journals
//! - [`JournalEntry`] — a single entry read back from the journal
//! - `FileJournal` — memory-mapped file journal implementation (requires `journal` feature)
//!
//! # Feature Gate
//!
//! The `FileJournal` implementation requires the `journal` feature:
//!
//! ```toml
//! [dependencies]
//! orderbook-rs = { version = "0.6", features = ["journal"] }
//! ```
//!
//! The sequencer types and [`Journal`] trait are always available.

pub mod error;
pub mod types;

#[cfg(feature = "journal")]
pub mod file_journal;

pub mod journal;

pub use error::JournalError;
#[cfg(feature = "journal")]
pub use file_journal::FileJournal;
pub use journal::{
    ENTRY_CRC_SIZE, ENTRY_HEADER_SIZE, ENTRY_OVERHEAD, Journal, JournalEntry, JournalReadIter,
};
pub use types::{SequencerCommand, SequencerEvent, SequencerResult};
