//! Error types for the journal subsystem.
//!
//! [`JournalError`] covers all failure modes of the append-only event
//! journal, including I/O errors, corruption, and capacity issues.

use std::fmt;
use std::path::PathBuf;

/// Errors that can occur within the journal subsystem.
#[derive(Debug)]
#[non_exhaustive]
pub enum JournalError {
    /// An I/O error occurred while reading or writing journal files.
    Io {
        /// The underlying I/O error message.
        message: String,
        /// The file path involved, if known.
        path: Option<PathBuf>,
    },

    /// A journal entry failed CRC32 integrity verification.
    CorruptEntry {
        /// The sequence number of the corrupt entry.
        sequence: u64,
        /// The expected CRC32 checksum.
        expected_crc: u32,
        /// The actual CRC32 checksum computed from the entry bytes.
        actual_crc: u32,
    },

    /// The journal entry payload could not be deserialized.
    DeserializationError {
        /// The sequence number of the entry that failed to deserialize.
        sequence: u64,
        /// The underlying deserialization error message.
        message: String,
    },

    /// The journal entry payload could not be serialized.
    SerializationError {
        /// The underlying serialization error message.
        message: String,
    },

    /// A segment file is too small to hold the entry being appended.
    EntryTooLarge {
        /// The size of the serialized entry in bytes.
        entry_bytes: usize,
        /// The maximum segment size in bytes.
        segment_size: usize,
    },

    /// The journal directory does not exist or is not accessible.
    InvalidDirectory {
        /// The path that was expected to be a valid directory.
        path: PathBuf,
    },

    /// An internal mutex was poisoned (another thread panicked while
    /// holding the lock).
    MutexPoisoned,

    /// The requested sequence number was not found in the journal.
    SequenceNotFound {
        /// The sequence number that was requested.
        sequence: u64,
    },

    /// The journal entry has an invalid header (truncated or malformed).
    InvalidEntryHeader {
        /// Byte offset within the segment where the error occurred.
        offset: usize,
        /// Description of the header problem.
        message: String,
    },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::Io { message, path } => {
                if let Some(p) = path {
                    write!(f, "journal I/O error at {}: {message}", p.display())
                } else {
                    write!(f, "journal I/O error: {message}")
                }
            }
            JournalError::CorruptEntry {
                sequence,
                expected_crc,
                actual_crc,
            } => {
                write!(
                    f,
                    "corrupt journal entry at sequence {sequence}: \
                     expected CRC {expected_crc:#010x}, got {actual_crc:#010x}"
                )
            }
            JournalError::DeserializationError { sequence, message } => {
                write!(
                    f,
                    "journal deserialization error at sequence {sequence}: {message}"
                )
            }
            JournalError::SerializationError { message } => {
                write!(f, "journal serialization error: {message}")
            }
            JournalError::EntryTooLarge {
                entry_bytes,
                segment_size,
            } => {
                write!(
                    f,
                    "journal entry too large: {entry_bytes} bytes exceeds \
                     segment size {segment_size} bytes"
                )
            }
            JournalError::InvalidDirectory { path } => {
                write!(f, "invalid journal directory: {}", path.display())
            }
            JournalError::MutexPoisoned => {
                write!(f, "journal internal mutex poisoned")
            }
            JournalError::SequenceNotFound { sequence } => {
                write!(f, "sequence {sequence} not found in journal")
            }
            JournalError::InvalidEntryHeader { offset, message } => {
                write!(
                    f,
                    "invalid journal entry header at offset {offset}: {message}"
                )
            }
        }
    }
}

impl std::error::Error for JournalError {}

impl From<std::io::Error> for JournalError {
    #[cold]
    fn from(err: std::io::Error) -> Self {
        JournalError::Io {
            message: err.to_string(),
            path: None,
        }
    }
}
