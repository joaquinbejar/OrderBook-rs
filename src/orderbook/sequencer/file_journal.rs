//! Memory-mapped file journal implementation.
//!
//! [`FileJournal`] persists [`SequencerEvent`] instances to append-only,
//! memory-mapped segment files on disk. Each segment is pre-allocated to a
//! configurable size (default 256 MB) and rotated when full.
//!
//! # On-Disk Entry Format (little-endian)
//!
//! ```text
//! [4 bytes: entry_length][8 bytes: sequence_num][8 bytes: timestamp_ns]
//! [N bytes: JSON payload][4 bytes: CRC32]
//! ```
//!
//! - `entry_length` — total bytes after itself (sequence + timestamp +
//!   payload + CRC = 20 + N).
//! - CRC32 covers: sequence_num ‖ timestamp_ns ‖ payload (not
//!   `entry_length`).
//!
//! # Segment Files
//!
//! Segments are named `segment-{start_sequence:020}.journal` and stored in
//! the configured journal directory. Archived segments are renamed to
//! `.journal.archived`.

use super::error::JournalError;
use super::journal::{ENTRY_CRC_SIZE, ENTRY_HEADER_SIZE, Journal, JournalEntry, JournalReadIter};
use super::types::SequencerEvent;
use memmap2::MmapMut;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::warn;

/// Default segment size in bytes (256 MB).
const DEFAULT_SEGMENT_SIZE: usize = 256 * 1024 * 1024;

/// Manages writing to a single memory-mapped segment file.
struct SegmentWriter {
    /// The memory-mapped region for this segment.
    mmap: MmapMut,
    /// Current write position within the segment (bytes).
    write_pos: usize,
    /// Total capacity of the segment in bytes.
    capacity: usize,
    /// Path to the segment file on disk.
    path: PathBuf,
}

impl SegmentWriter {
    /// Create a new segment file and memory-map it.
    ///
    /// The file is pre-allocated to `capacity` bytes and filled with zeros.
    fn create(path: &Path, capacity: usize) -> Result<Self, JournalError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| JournalError::Io {
                message: e.to_string(),
                path: Some(path.to_path_buf()),
            })?;

        file.set_len(capacity as u64)
            .map_err(|e| JournalError::Io {
                message: e.to_string(),
                path: Some(path.to_path_buf()),
            })?;

        // SAFETY: The file is exclusively owned by this process and will not
        // be truncated or modified externally while the mmap is active.
        let mmap = unsafe {
            MmapMut::map_mut(&file).map_err(|e| JournalError::Io {
                message: e.to_string(),
                path: Some(path.to_path_buf()),
            })?
        };

        Ok(Self {
            mmap,
            write_pos: 0,
            capacity,
            path: path.to_path_buf(),
        })
    }

    /// Open an existing segment file for appending.
    ///
    /// Scans entries to find the current write position.
    fn open_existing(path: &Path) -> Result<Self, JournalError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| JournalError::Io {
                message: e.to_string(),
                path: Some(path.to_path_buf()),
            })?;

        let metadata = file.metadata().map_err(|e| JournalError::Io {
            message: e.to_string(),
            path: Some(path.to_path_buf()),
        })?;

        let capacity = metadata.len() as usize;

        // SAFETY: The file is exclusively owned by this process and will not
        // be truncated or modified externally while the mmap is active.
        let mmap = unsafe {
            MmapMut::map_mut(&file).map_err(|e| JournalError::Io {
                message: e.to_string(),
                path: Some(path.to_path_buf()),
            })?
        };

        // Scan to find the write position (end of last valid entry)
        let write_pos = scan_write_position(&mmap, capacity);

        Ok(Self {
            mmap,
            write_pos,
            capacity,
            path: path.to_path_buf(),
        })
    }

    /// Returns the remaining capacity in this segment.
    #[inline]
    fn remaining(&self) -> usize {
        self.capacity.saturating_sub(self.write_pos)
    }

    /// Write a raw entry to the segment at the current position.
    ///
    /// Returns `Ok(())` after flushing the written range to disk.
    fn write_entry(&mut self, entry_bytes: &[u8]) -> Result<(), JournalError> {
        let end =
            self.write_pos
                .checked_add(entry_bytes.len())
                .ok_or(JournalError::EntryTooLarge {
                    entry_bytes: entry_bytes.len(),
                    segment_size: self.capacity,
                })?;

        if end > self.capacity {
            return Err(JournalError::EntryTooLarge {
                entry_bytes: entry_bytes.len(),
                segment_size: self.capacity,
            });
        }

        self.mmap[self.write_pos..end].copy_from_slice(entry_bytes);
        self.mmap
            .flush_range(self.write_pos, entry_bytes.len())
            .map_err(|e| JournalError::Io {
                message: e.to_string(),
                path: Some(self.path.clone()),
            })?;
        self.write_pos = end;
        Ok(())
    }
}

/// A memory-mapped, append-only event journal with segment rotation.
///
/// `FileJournal` stores [`SequencerEvent`] instances in pre-allocated
/// segment files using memory-mapped I/O. Each entry is checksummed with
/// CRC32 for corruption detection.
///
/// # Segment Rotation
///
/// When the current segment cannot fit the next entry, a new segment file
/// is created and the write position resets. Old segments remain on disk
/// for reading until explicitly archived via
/// [`archive_segments_before`](FileJournal::archive_segments_before).
///
/// # Thread Safety
///
/// The internal write state is protected by a [`Mutex`]. The intended
/// usage is single-writer (Sequencer thread) with concurrent readers
/// (replay). The Mutex is uncontended in the single-writer case.
///
/// # Example
///
/// ```rust,no_run
/// use orderbook_rs::orderbook::sequencer::{FileJournal, Journal, SequencerEvent};
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let journal: FileJournal<()> = FileJournal::open("/tmp/journal")?;
/// // Use journal.append(&event) in the Sequencer run loop
/// # Ok(())
/// # }
/// ```
pub struct FileJournal<T> {
    /// Directory containing segment files.
    dir: PathBuf,
    /// The active segment being written to.
    writer: Mutex<SegmentWriter>,
    /// Maximum size of each segment file in bytes.
    segment_size: usize,
    /// The sequence number of the first entry in the current segment.
    segment_start_seq: Mutex<u64>,
    /// The last sequence number written to the journal.
    last_seq: Mutex<Option<u64>>,
    /// Marker for the generic event payload type.
    _phantom: PhantomData<T>,
}

impl<T> FileJournal<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + 'static,
{
    /// Open or create a journal in the given directory.
    ///
    /// If the directory contains existing segment files, the journal
    /// resumes from the latest segment. Otherwise, a new segment is
    /// created starting at sequence 0.
    ///
    /// # Arguments
    ///
    /// * `dir` — path to the journal directory (created if it does not
    ///   exist)
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] if the directory cannot be created or
    /// existing segments cannot be opened.
    pub fn open<P: AsRef<Path>>(dir: P) -> Result<Self, JournalError> {
        Self::open_with_segment_size(dir, DEFAULT_SEGMENT_SIZE)
    }

    /// Open or create a journal with a custom segment size.
    ///
    /// # Arguments
    ///
    /// * `dir` — path to the journal directory
    /// * `segment_size` — maximum size of each segment file in bytes
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] if the directory cannot be created or
    /// existing segments cannot be opened.
    pub fn open_with_segment_size<P: AsRef<Path>>(
        dir: P,
        segment_size: usize,
    ) -> Result<Self, JournalError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| JournalError::Io {
            message: e.to_string(),
            path: Some(dir.clone()),
        })?;

        // Find existing segments sorted by start sequence
        let mut segments = list_segments(&dir)?;
        segments.sort();

        let (writer, segment_start_seq, last_seq) = if let Some(latest) = segments.last() {
            let path = segment_path(&dir, *latest);
            let seg = SegmentWriter::open_existing(&path)?;
            let last = scan_last_sequence(&seg.mmap, seg.write_pos);
            (seg, *latest, last)
        } else {
            // No existing segments — create the first one
            let path = segment_path(&dir, 0);
            let seg = SegmentWriter::create(&path, segment_size)?;
            (seg, 0, None)
        };

        Ok(Self {
            dir,
            writer: Mutex::new(writer),
            segment_size,
            segment_start_seq: Mutex::new(segment_start_seq),
            last_seq: Mutex::new(last_seq),
            _phantom: PhantomData,
        })
    }

    /// Archive all segment files whose start sequence is strictly less
    /// than `before_sequence`.
    ///
    /// Archived segments are renamed from `.journal` to
    /// `.journal.archived` and are excluded from future reads.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] if any segment file cannot be renamed.
    pub fn archive_segments_before(&self, before_sequence: u64) -> Result<usize, JournalError> {
        let segments = list_segments(&self.dir)?;
        let mut archived = 0usize;

        // Never archive the active segment — the writer is still appending to it.
        let active_start = self
            .segment_start_seq
            .lock()
            .map_err(|_| JournalError::MutexPoisoned)?;

        for start_seq in segments {
            if start_seq < before_sequence && start_seq != *active_start {
                let src = segment_path(&self.dir, start_seq);
                let mut dst = src.clone();
                dst.set_extension("journal.archived");
                fs::rename(&src, &dst).map_err(|e| JournalError::Io {
                    message: e.to_string(),
                    path: Some(src),
                })?;
                // checked_add (not saturating) so an overflow surfaces as a
                // typed error rather than silently capping the tally.
                archived = archived
                    .checked_add(1)
                    .ok_or(JournalError::CounterOverflow {
                        counter: "archived segment count",
                    })?;
            }
        }

        Ok(archived)
    }

    /// Rotate to a new segment file starting at the given sequence.
    fn rotate_segment(
        &self,
        writer: &mut SegmentWriter,
        start_seq: u64,
    ) -> Result<(), JournalError> {
        // Flush the old segment's mmap before rotating away from it.
        let old_path = writer.path.clone();
        writer.mmap.flush().map_err(|e| JournalError::Io {
            message: e.to_string(),
            path: Some(old_path.clone()),
        })?;

        // Create the new segment and swap it in.
        let new_path = segment_path(&self.dir, start_seq);
        let new_writer = SegmentWriter::create(&new_path, self.segment_size)?;
        *writer = new_writer;

        // NOTE: we deliberately do NOT `set_len` the old segment down to its
        // used size. A concurrent reader (`read_from` / `verify_integrity`) may
        // already have it mmap'd at full capacity, and shrinking a mapped file
        // makes touching pages past the new EOF undefined behaviour (SIGBUS on
        // Unix) — exactly what the `SegmentWriter` SAFETY comments rely on never
        // happening. The unused tail is a sparse hole (the segment was grown
        // with `set_len`, never written), so it costs no physical disk; there is
        // nothing to reclaim.

        // Advance segment_start_seq, surfacing a poisoned lock rather than
        // swallowing it. Leaving it stale would let `archive_segments_before`
        // archive the now-active segment.
        let mut start = self
            .segment_start_seq
            .lock()
            .map_err(|_| JournalError::MutexPoisoned)?;
        *start = start_seq;

        Ok(())
    }

    /// Serialize and encode a single event into the on-disk binary format.
    fn encode_entry(event: &SequencerEvent<T>) -> Result<Vec<u8>, JournalError> {
        let payload = serde_json::to_vec(event).map_err(|e| JournalError::SerializationError {
            message: e.to_string(),
        })?;

        let payload_len = payload.len();
        // entry_length = 8 (seq) + 8 (ts) + payload_len + 4 (crc)
        let entry_length = 8u32
            .checked_add(8)
            .and_then(|v| v.checked_add(payload_len as u32))
            .and_then(|v| v.checked_add(4))
            .ok_or(JournalError::SerializationError {
                message: "entry size overflow".to_string(),
            })?;

        let total_bytes =
            (entry_length as usize)
                .checked_add(4)
                .ok_or(JournalError::SerializationError {
                    message: "total entry size overflow".to_string(),
                })?;

        let mut buf = Vec::with_capacity(total_bytes);

        // Write entry_length (4 bytes LE)
        buf.write_all(&entry_length.to_le_bytes()).map_err(|e| {
            JournalError::SerializationError {
                message: e.to_string(),
            }
        })?;

        // Write sequence_num (8 bytes LE)
        buf.write_all(&event.sequence_num.to_le_bytes())
            .map_err(|e| JournalError::SerializationError {
                message: e.to_string(),
            })?;

        // Write timestamp_ns (8 bytes LE)
        buf.write_all(&event.timestamp_ns.to_le_bytes())
            .map_err(|e| JournalError::SerializationError {
                message: e.to_string(),
            })?;

        // Write payload
        buf.write_all(&payload)
            .map_err(|e| JournalError::SerializationError {
                message: e.to_string(),
            })?;

        // Compute CRC32 over (sequence_num ‖ timestamp_ns ‖ payload). The CRC
        // is appended *after* this point, so `buf[4..]` is exactly that range
        // and does not (and must not) cover the CRC field itself — the same
        // range `entry_crc_valid` and `verify_integrity` re-check on read.
        let crc = crc32fast::hash(&buf[4..]);

        // Write CRC32 (4 bytes LE)
        buf.write_all(&crc.to_le_bytes())
            .map_err(|e| JournalError::SerializationError {
                message: e.to_string(),
            })?;

        Ok(buf)
    }
}

impl<T> Journal<T> for FileJournal<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + 'static,
{
    fn append(&self, event: &SequencerEvent<T>) -> Result<(), JournalError> {
        let entry_bytes = Self::encode_entry(event)?;

        let mut writer = self
            .writer
            .lock()
            .map_err(|_| JournalError::MutexPoisoned)?;

        // Rotate if the current segment cannot fit this entry
        if writer.remaining() < entry_bytes.len() {
            self.rotate_segment(&mut writer, event.sequence_num)?;
        }

        // Still too large for a fresh segment? (single entry > segment size)
        if writer.remaining() < entry_bytes.len() {
            return Err(JournalError::EntryTooLarge {
                entry_bytes: entry_bytes.len(),
                segment_size: self.segment_size,
            });
        }

        writer.write_entry(&entry_bytes)?;

        // Advance last_seq, surfacing a poisoned lock rather than swallowing it.
        // Swallowing would leave `last_sequence()` under-reporting and break
        // replay bounds / gap detection; returning the error here means append
        // never reports success while last_seq is left unadvanced.
        let mut last = self
            .last_seq
            .lock()
            .map_err(|_| JournalError::MutexPoisoned)?;
        *last = Some(event.sequence_num);

        Ok(())
    }

    fn read_from(&self, sequence: u64) -> Result<JournalReadIter<T>, JournalError> {
        // Collect all segment files sorted by start sequence
        let mut segments = list_segments(&self.dir)?;
        segments.sort();

        // Find the segment that could contain the requested sequence.
        // The right segment has the largest start_seq <= sequence.
        let start_idx = match segments.binary_search(&sequence) {
            Ok(idx) => idx,
            Err(0) => 0,
            Err(idx) => idx.saturating_sub(1),
        };

        let dir = self.dir.clone();
        let segments_from: Vec<u64> = segments.into_iter().skip(start_idx).collect();

        let iter = SegmentIterator::<T> {
            dir,
            segments: segments_from,
            segment_idx: 0,
            offset: 0,
            mmap: None,
            mmap_len: 0,
            start_sequence: sequence,
            started: false,
            _phantom: PhantomData,
        };

        Ok(Box::new(iter))
    }

    fn last_sequence(&self) -> Option<u64> {
        // The `Journal` trait returns `Option`, so a poisoned `last_seq` lock
        // surfaces here as `None` rather than a typed error (unlike `append`,
        // which returns `JournalError::MutexPoisoned`). Either way the on-disk
        // state is authoritative: a reopen reconstructs `last_seq` from the
        // CRC-validated tail scan, so a poisoned in-memory value is transient.
        self.last_seq.lock().ok().and_then(|guard| *guard)
    }

    fn verify_integrity(&self) -> Result<(), JournalError> {
        let mut segments = list_segments(&self.dir)?;
        segments.sort();

        for start_seq in segments {
            let path = segment_path(&self.dir, start_seq);
            let file = File::open(&path).map_err(|e| JournalError::Io {
                message: e.to_string(),
                path: Some(path.clone()),
            })?;

            // SAFETY: Read-only mapping of a file we just opened; file is not
            // modified concurrently (single-writer pattern).
            let mmap = unsafe {
                memmap2::Mmap::map(&file).map_err(|e| JournalError::Io {
                    message: e.to_string(),
                    path: Some(path.clone()),
                })?
            };

            let data = &mmap[..];
            let mut offset = 0usize;

            while offset.checked_add(ENTRY_HEADER_SIZE).is_some()
                && offset + ENTRY_HEADER_SIZE <= data.len()
            {
                // Read entry_length
                let el_bytes =
                    data.get(offset..offset + 4)
                        .ok_or(JournalError::InvalidEntryHeader {
                            offset,
                            message: "truncated entry_length".to_string(),
                        })?;
                let entry_length =
                    u32::from_le_bytes([el_bytes[0], el_bytes[1], el_bytes[2], el_bytes[3]])
                        as usize;

                if entry_length == 0 {
                    break; // End of written data (zero-filled region)
                }

                let entry_end = offset
                    .checked_add(4)
                    .and_then(|v| v.checked_add(entry_length));
                let entry_end = match entry_end {
                    Some(end) if end <= data.len() => end,
                    _ => {
                        return Err(JournalError::InvalidEntryHeader {
                            offset,
                            message: "truncated entry (extends beyond segment data)".to_string(),
                        });
                    }
                };

                // Verify CRC
                let crc_start = entry_end.checked_sub(ENTRY_CRC_SIZE).ok_or(
                    JournalError::InvalidEntryHeader {
                        offset,
                        message: "entry too small for CRC".to_string(),
                    },
                )?;

                let payload_start =
                    offset
                        .checked_add(4)
                        .ok_or(JournalError::InvalidEntryHeader {
                            offset,
                            message: "offset overflow".to_string(),
                        })?;

                let crc_bytes =
                    data.get(crc_start..entry_end)
                        .ok_or(JournalError::InvalidEntryHeader {
                            offset,
                            message: "truncated CRC".to_string(),
                        })?;
                let stored_crc =
                    u32::from_le_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]);

                let checksummed_data =
                    data.get(payload_start..crc_start)
                        .ok_or(JournalError::InvalidEntryHeader {
                            offset,
                            message: "truncated payload".to_string(),
                        })?;
                let computed_crc = crc32fast::hash(checksummed_data);

                if stored_crc != computed_crc {
                    // Read sequence_num for the error message
                    let seq_bytes = data.get(payload_start..payload_start + 8).ok_or(
                        JournalError::InvalidEntryHeader {
                            offset,
                            message: "truncated sequence_num".to_string(),
                        },
                    )?;
                    let seq = u64::from_le_bytes([
                        seq_bytes[0],
                        seq_bytes[1],
                        seq_bytes[2],
                        seq_bytes[3],
                        seq_bytes[4],
                        seq_bytes[5],
                        seq_bytes[6],
                        seq_bytes[7],
                    ]);

                    return Err(JournalError::CorruptEntry {
                        sequence: seq,
                        expected_crc: stored_crc,
                        actual_crc: computed_crc,
                    });
                }

                offset = entry_end;
            }
        }

        Ok(())
    }
}

impl<T> std::fmt::Debug for FileJournal<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileJournal")
            .field("dir", &self.dir)
            .field("segment_size", &self.segment_size)
            .field("last_seq", &self.last_seq.lock().ok().and_then(|g| *g))
            .finish()
    }
}

// ─── Iteration ──────────────────────────────────────────────────────────────

/// An iterator over journal entries across multiple segment files.
struct SegmentIterator<T> {
    dir: PathBuf,
    segments: Vec<u64>,
    segment_idx: usize,
    offset: usize,
    mmap: Option<memmap2::Mmap>,
    mmap_len: usize,
    start_sequence: u64,
    started: bool,
    _phantom: PhantomData<T>,
}

impl<T> SegmentIterator<T>
where
    T: for<'de> Deserialize<'de> + Clone + 'static,
{
    /// Load the next segment's mmap. Returns false if no more segments.
    fn load_next_segment(&mut self) -> Result<bool, JournalError> {
        if self.segment_idx >= self.segments.len() {
            return Ok(false);
        }

        let start_seq = self.segments[self.segment_idx];
        let path = segment_path(&self.dir, start_seq);
        // checked_add (not saturating) so an overflow surfaces as a typed error
        // rather than silently stalling the segment cursor.
        self.segment_idx =
            self.segment_idx
                .checked_add(1)
                .ok_or(JournalError::CounterOverflow {
                    counter: "segment index",
                })?;
        self.offset = 0;

        let file = File::open(&path).map_err(|e| JournalError::Io {
            message: e.to_string(),
            path: Some(path.clone()),
        })?;

        // SAFETY: Read-only mapping; single-writer pattern ensures the
        // segment is not modified concurrently by another writer.
        let mmap = unsafe {
            memmap2::Mmap::map(&file).map_err(|e| JournalError::Io {
                message: e.to_string(),
                path: Some(path),
            })?
        };

        self.mmap_len = mmap.len();
        self.mmap = Some(mmap);
        Ok(true)
    }

    /// Try to decode the next entry from the current mmap at `self.offset`.
    fn decode_next(&mut self) -> Option<Result<JournalEntry<T>, JournalError>> {
        let mmap = self.mmap.as_ref()?;
        let data = &mmap[..];

        if self.offset.checked_add(ENTRY_HEADER_SIZE).is_none()
            || self.offset + ENTRY_HEADER_SIZE > data.len()
        {
            return None;
        }

        // Read entry_length
        let el_bytes = data.get(self.offset..self.offset + 4)?;
        let entry_length =
            u32::from_le_bytes([el_bytes[0], el_bytes[1], el_bytes[2], el_bytes[3]]) as usize;

        if entry_length == 0 {
            return None; // End of written data
        }

        let entry_end = self.offset.checked_add(4)?.checked_add(entry_length)?;
        if entry_end > data.len() {
            return None; // Truncated
        }

        let payload_start = self.offset.checked_add(4)?;
        let crc_start = entry_end.checked_sub(ENTRY_CRC_SIZE)?;

        // Read stored CRC
        let crc_bytes = data.get(crc_start..entry_end)?;
        let stored_crc =
            u32::from_le_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]);

        // Verify CRC
        let checksummed_data = data.get(payload_start..crc_start)?;
        let computed_crc = crc32fast::hash(checksummed_data);

        if stored_crc != computed_crc {
            let seq_bytes = data.get(payload_start..payload_start + 8)?;
            let seq = u64::from_le_bytes([
                seq_bytes[0],
                seq_bytes[1],
                seq_bytes[2],
                seq_bytes[3],
                seq_bytes[4],
                seq_bytes[5],
                seq_bytes[6],
                seq_bytes[7],
            ]);
            self.offset = entry_end;
            return Some(Err(JournalError::CorruptEntry {
                sequence: seq,
                expected_crc: stored_crc,
                actual_crc: computed_crc,
            }));
        }

        // Read sequence_num (first 8 bytes after entry_length)
        let seq_bytes = data.get(payload_start..payload_start + 8)?;
        let sequence_num = u64::from_le_bytes([
            seq_bytes[0],
            seq_bytes[1],
            seq_bytes[2],
            seq_bytes[3],
            seq_bytes[4],
            seq_bytes[5],
            seq_bytes[6],
            seq_bytes[7],
        ]);

        // Deserialize the payload (between timestamp_ns and CRC)
        // The full payload region is: payload_start .. crc_start
        // But we stored sequence_num + timestamp_ns + JSON payload
        // The JSON payload starts at payload_start + 8 (seq) + 8 (ts)
        let json_start = payload_start.checked_add(16)?;
        let json_data = data.get(json_start..crc_start)?;

        let event: SequencerEvent<T> = match serde_json::from_slice(json_data) {
            Ok(ev) => ev,
            Err(e) => {
                self.offset = entry_end;
                return Some(Err(JournalError::DeserializationError {
                    sequence: sequence_num,
                    message: e.to_string(),
                }));
            }
        };

        self.offset = entry_end;

        Some(Ok(JournalEntry { event, stored_crc }))
    }
}

impl<T> Iterator for SegmentIterator<T>
where
    T: for<'de> Deserialize<'de> + Clone + 'static,
{
    type Item = Result<JournalEntry<T>, JournalError>;

    fn next(&mut self) -> Option<Self::Item> {
        // Load the first segment if not yet started
        if !self.started {
            self.started = true;
            match self.load_next_segment() {
                Ok(true) => {}
                Ok(false) => return None,
                Err(e) => return Some(Err(e)),
            }
        }

        loop {
            // Try to read from the current segment
            if let Some(result) = self.decode_next() {
                if let Ok(entry) = &result {
                    // Skip entries before the requested start sequence
                    if entry.event.sequence_num < self.start_sequence {
                        continue;
                    }
                }
                return Some(result);
            }

            // Current segment exhausted — try the next one
            match self.load_next_segment() {
                Ok(true) => continue,
                Ok(false) => return None,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Build the path for a segment file given its start sequence.
fn segment_path(dir: &Path, start_sequence: u64) -> PathBuf {
    dir.join(format!("segment-{start_sequence:020}.journal"))
}

/// List all active (non-archived) segment start sequences in the directory.
fn list_segments(dir: &Path) -> Result<Vec<u64>, JournalError> {
    let mut seqs = Vec::new();

    let entries = fs::read_dir(dir).map_err(|e| JournalError::Io {
        message: e.to_string(),
        path: Some(dir.to_path_buf()),
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| JournalError::Io {
            message: e.to_string(),
            path: Some(dir.to_path_buf()),
        })?;

        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Match pattern: segment-{seq}.journal (NOT .journal.archived)
        if let Some(rest) = name_str.strip_prefix("segment-")
            && let Some(seq_str) = rest.strip_suffix(".journal")
            && let Ok(seq) = seq_str.parse::<u64>()
        {
            seqs.push(seq);
        }
    }

    Ok(seqs)
}

/// Verifies the CRC32 of the entry occupying `data[offset..entry_end]`.
///
/// `entry_end` must already be bounds-checked against `data`. The CRC covers
/// `sequence_num ‖ timestamp_ns ‖ payload` (the bytes between the 4-byte
/// `entry_length` header and the trailing 4-byte CRC), matching the layout
/// written by [`FileJournal::encode_entry`]. Returns `false` for a torn entry
/// (payload or CRC damaged by a crash mid-flush) or any out-of-bounds slice.
fn entry_crc_valid(data: &[u8], offset: usize, entry_end: usize) -> bool {
    let crc_start = match entry_end.checked_sub(ENTRY_CRC_SIZE) {
        Some(s) => s,
        None => return false,
    };
    let payload_start = match offset.checked_add(4) {
        Some(p) => p,
        None => return false,
    };
    if payload_start > crc_start || entry_end > data.len() {
        return false;
    }
    let (Some(crc_bytes), Some(checksummed)) = (
        data.get(crc_start..entry_end),
        data.get(payload_start..crc_start),
    ) else {
        return false;
    };
    let stored_crc = u32::from_le_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]);
    crc32fast::hash(checksummed) == stored_crc
}

/// Scan a memory-mapped segment to find the write position (byte offset of the
/// end of valid written data).
///
/// Stops at the first zero `entry_length` (the zero-filled tail), at a header
/// that claims to extend beyond the segment, or — crucially — at the first
/// entry whose CRC does not validate. A crash mid-flush can leave a torn tail
/// entry with an intact `entry_length`/`sequence_num` header but a damaged
/// payload/CRC; treating that entry's start as end-of-valid-data means the next
/// append truncates over the corruption rather than resuming on top of it, and
/// [`scan_last_sequence`] (which scans only up to this position) reports the
/// last decodable sequence.
fn scan_write_position(data: &[u8], capacity: usize) -> usize {
    let mut offset = 0usize;

    while let Some(end) = offset.checked_add(4) {
        if end > capacity || end > data.len() {
            break;
        }

        let el_bytes = match data.get(offset..end) {
            Some(b) => b,
            None => break,
        };
        let entry_length =
            u32::from_le_bytes([el_bytes[0], el_bytes[1], el_bytes[2], el_bytes[3]]) as usize;

        if entry_length == 0 {
            break;
        }

        let entry_end = match offset
            .checked_add(4)
            .and_then(|v| v.checked_add(entry_length))
        {
            Some(end) if end <= capacity && end <= data.len() => end,
            _ => break,
        };

        if !entry_crc_valid(data, offset, entry_end) {
            warn!(
                offset,
                "torn journal tail detected on reopen; truncating to the last good entry"
            );
            break;
        }

        offset = entry_end;
    }

    offset
}

/// Scan a segment to find the last sequence number written.
fn scan_last_sequence(data: &[u8], write_pos: usize) -> Option<u64> {
    let mut offset = 0usize;
    let mut last_seq: Option<u64> = None;

    while offset.checked_add(ENTRY_HEADER_SIZE).is_some() && offset + ENTRY_HEADER_SIZE <= write_pos
    {
        let el_bytes = data.get(offset..offset + 4)?;
        let entry_length =
            u32::from_le_bytes([el_bytes[0], el_bytes[1], el_bytes[2], el_bytes[3]]) as usize;

        if entry_length == 0 {
            break;
        }

        let entry_end = offset.checked_add(4)?.checked_add(entry_length)?;
        if entry_end > write_pos {
            break;
        }

        // Read sequence_num at offset+4
        let seq_start = offset.checked_add(4)?;
        let seq_bytes = data.get(seq_start..seq_start + 8)?;
        let seq = u64::from_le_bytes([
            seq_bytes[0],
            seq_bytes[1],
            seq_bytes[2],
            seq_bytes[3],
            seq_bytes[4],
            seq_bytes[5],
            seq_bytes[6],
            seq_bytes[7],
        ]);

        last_seq = Some(seq);
        offset = entry_end;
    }

    last_seq
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orderbook::sequencer::types::{SequencerCommand, SequencerResult};
    use pricelevel::Id;

    fn make_event(seq: u64) -> SequencerEvent<()> {
        SequencerEvent {
            sequence_num: seq,
            timestamp_ns: 1_700_000_000_000_000_000u64.checked_add(seq).unwrap_or(0),
            command: SequencerCommand::CancelOrder(Id::new_uuid()),
            result: SequencerResult::OrderCancelled {
                order_id: Id::new_uuid(),
            },
        }
    }

    #[test]
    fn test_encode_entry_and_decode() {
        let event = make_event(42);
        let entry_bytes = FileJournal::<()>::encode_entry(&event);
        assert!(entry_bytes.is_ok());
        let buf = entry_bytes.unwrap_or_default();
        assert!(!buf.is_empty());

        // Verify entry_length field
        let entry_length = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        assert_eq!(entry_length + 4, buf.len());

        // Verify sequence_num
        let seq = u64::from_le_bytes([
            buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11],
        ]);
        assert_eq!(seq, 42);
    }

    #[test]
    fn test_write_and_read_single_entry() {
        let dir = tempfile::tempdir();
        assert!(dir.is_ok());
        let dir = dir.unwrap_or_else(|_| panic!("tempdir"));

        let journal = FileJournal::<()>::open(dir.path());
        assert!(journal.is_ok());
        let journal = journal.unwrap_or_else(|_| panic!("open"));

        let event = make_event(0);
        let result = journal.append(&event);
        assert!(result.is_ok());

        assert_eq!(journal.last_sequence(), Some(0));

        let entries: Vec<_> = journal
            .read_from(0)
            .unwrap_or_else(|_| panic!("read_from"))
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_ok());
        let entry = entries[0].as_ref().unwrap_or_else(|_| panic!("entry"));
        assert_eq!(entry.event.sequence_num, 0);
    }

    #[test]
    fn test_write_and_read_multiple_entries() {
        let dir = tempfile::tempdir();
        assert!(dir.is_ok());
        let dir = dir.unwrap_or_else(|_| panic!("tempdir"));

        let journal = FileJournal::<()>::open(dir.path());
        assert!(journal.is_ok());
        let journal = journal.unwrap_or_else(|_| panic!("open"));

        for i in 0..10 {
            let event = make_event(i);
            let result = journal.append(&event);
            assert!(result.is_ok());
        }

        assert_eq!(journal.last_sequence(), Some(9));

        // Read from sequence 5
        let entries: Vec<_> = journal
            .read_from(5)
            .unwrap_or_else(|_| panic!("read_from"))
            .collect();
        assert_eq!(entries.len(), 5);
        for (i, entry) in entries.iter().enumerate() {
            assert!(entry.is_ok());
            let e = entry.as_ref().unwrap_or_else(|_| panic!("entry"));
            assert_eq!(e.event.sequence_num, 5 + i as u64);
        }
    }

    #[test]
    fn test_read_from_empty_journal() {
        let dir = tempfile::tempdir();
        assert!(dir.is_ok());
        let dir = dir.unwrap_or_else(|_| panic!("tempdir"));

        let journal = FileJournal::<()>::open(dir.path());
        assert!(journal.is_ok());
        let journal = journal.unwrap_or_else(|_| panic!("open"));

        assert_eq!(journal.last_sequence(), None);

        let entries: Vec<_> = journal
            .read_from(0)
            .unwrap_or_else(|_| panic!("read_from"))
            .collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_segment_rotation() {
        let dir = tempfile::tempdir();
        assert!(dir.is_ok());
        let dir = dir.unwrap_or_else(|_| panic!("tempdir"));

        // Use a very small segment size to force rotation
        let journal = FileJournal::<()>::open_with_segment_size(dir.path(), 512);
        assert!(journal.is_ok());
        let journal = journal.unwrap_or_else(|_| panic!("open"));

        // Write enough entries to force at least one rotation
        for i in 0..20 {
            let event = make_event(i);
            let result = journal.append(&event);
            assert!(result.is_ok());
        }

        assert_eq!(journal.last_sequence(), Some(19));

        // Verify all entries can be read back
        let entries: Vec<_> = journal
            .read_from(0)
            .unwrap_or_else(|_| panic!("read_from"))
            .collect();
        assert_eq!(entries.len(), 20);
        for (i, entry) in entries.iter().enumerate() {
            assert!(entry.is_ok());
            let e = entry.as_ref().unwrap_or_else(|_| panic!("entry"));
            assert_eq!(e.event.sequence_num, i as u64);
        }

        // Verify multiple segment files exist
        let segments = list_segments(dir.path());
        assert!(segments.is_ok());
        let segs = segments.unwrap_or_default();
        assert!(
            segs.len() > 1,
            "expected multiple segments, got {}",
            segs.len()
        );
    }

    #[test]
    fn test_verify_integrity_on_valid_journal() {
        let dir = tempfile::tempdir();
        assert!(dir.is_ok());
        let dir = dir.unwrap_or_else(|_| panic!("tempdir"));

        let journal = FileJournal::<()>::open(dir.path());
        assert!(journal.is_ok());
        let journal = journal.unwrap_or_else(|_| panic!("open"));

        for i in 0..5 {
            let event = make_event(i);
            let result = journal.append(&event);
            assert!(result.is_ok());
        }

        let integrity = journal.verify_integrity();
        assert!(integrity.is_ok());
    }

    #[test]
    fn test_verify_integrity_detects_corruption() {
        let dir = tempfile::tempdir();
        assert!(dir.is_ok());
        let dir = dir.unwrap_or_else(|_| panic!("tempdir"));

        let journal = FileJournal::<()>::open(dir.path());
        assert!(journal.is_ok());
        let journal = journal.unwrap_or_else(|_| panic!("open"));

        let event = make_event(0);
        let result = journal.append(&event);
        assert!(result.is_ok());

        // Verify integrity passes before corruption
        assert!(journal.verify_integrity().is_ok());

        // Drop the journal to release the mmap
        drop(journal);

        // Corrupt a byte in the payload region of the segment file
        let segments = list_segments(dir.path());
        assert!(segments.is_ok());
        let segs = segments.unwrap_or_default();
        assert!(!segs.is_empty());

        let seg_path = segment_path(dir.path(), segs[0]);
        let mut data = fs::read(&seg_path).unwrap_or_default();
        // Flip a byte in the payload area (after the header)
        if data.len() > 30 {
            data[25] ^= 0xFF;
        }
        fs::write(&seg_path, &data).unwrap_or_default();

        // Re-open and verify — should detect corruption
        let journal2 = FileJournal::<()>::open(dir.path());
        assert!(journal2.is_ok());
        let journal2 = journal2.unwrap_or_else(|_| panic!("reopen"));

        let integrity = journal2.verify_integrity();
        assert!(integrity.is_err());
        let err_msg = format!("{}", integrity.unwrap_err());
        assert!(err_msg.contains("corrupt journal entry"));
    }

    /// Walks the entry-length headers (the pre-CRC scan) to find the byte
    /// offset of the end of written data. Used by the torn-tail test to corrupt
    /// the final entry without depending on the CRC-aware scanner under test.
    fn written_len(data: &[u8]) -> usize {
        let mut off = 0usize;
        while off + 4 <= data.len() {
            let el = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
                as usize;
            if el == 0 {
                break;
            }
            match off.checked_add(4).and_then(|v| v.checked_add(el)) {
                Some(end) if end <= data.len() => off = end,
                _ => break,
            }
        }
        off
    }

    #[test]
    fn test_reopen_truncates_torn_tail_then_appends_and_replays() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| panic!("tempdir"));

        // Write three valid entries (seq 0, 1, 2).
        let journal = FileJournal::<()>::open(dir.path()).unwrap_or_else(|_| panic!("open"));
        for i in 0..3 {
            assert!(journal.append(&make_event(i)).is_ok());
        }
        assert_eq!(journal.last_sequence(), Some(2));
        drop(journal); // release the mmap

        // Corrupt the final entry's trailing CRC byte: a crash mid-flush leaves
        // the entry_length/sequence_num header intact but the payload/CRC torn.
        let segs = list_segments(dir.path()).unwrap_or_default();
        assert_eq!(segs.len(), 1);
        let seg_path = segment_path(dir.path(), segs[0]);
        let mut data = fs::read(&seg_path).unwrap_or_default();
        let write_pos = written_len(&data);
        assert!(write_pos > 0);
        data[write_pos - 1] ^= 0xFF; // flip the last CRC byte of the last entry
        fs::write(&seg_path, &data).unwrap_or_default();

        // Reopen: the scan must stop at the torn entry, so last_sequence reports
        // the last *good* entry (seq 1), not the torn seq 2.
        let journal2 = FileJournal::<()>::open(dir.path()).unwrap_or_else(|_| panic!("reopen"));
        assert_eq!(
            journal2.last_sequence(),
            Some(1),
            "torn tail must truncate to the last good entry"
        );

        // Appending after a torn-tail reopen overwrites the corrupt bytes, so a
        // subsequent integrity check and replay succeed.
        assert!(journal2.append(&make_event(2)).is_ok());
        assert_eq!(journal2.last_sequence(), Some(2));
        assert!(
            journal2.verify_integrity().is_ok(),
            "the torn entry must be overwritten by the new append"
        );

        let entries: Vec<_> = journal2
            .read_from(0)
            .unwrap_or_else(|_| panic!("read_from"))
            .collect();
        assert_eq!(entries.len(), 3, "0, 1, and the re-appended 2");
        for (i, entry) in entries.iter().enumerate() {
            let e = entry.as_ref().unwrap_or_else(|_| panic!("entry decodes"));
            assert_eq!(e.event.sequence_num, i as u64);
        }
    }

    /// Poison a mutex by panicking while holding its guard, so a later `lock()`
    /// returns `Err(PoisonError)`.
    fn poison<G>(mutex: &Mutex<G>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mutex.lock().unwrap_or_else(|_| panic!("lock to poison"));
            panic!("intentional poison");
        }));
    }

    #[test]
    fn test_append_surfaces_poisoned_last_seq() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| panic!("tempdir"));
        let journal = FileJournal::<()>::open(dir.path()).unwrap_or_else(|_| panic!("open"));

        poison(&journal.last_seq);

        // append must surface the poison as a typed error, not swallow it.
        match journal.append(&make_event(0)) {
            Err(JournalError::MutexPoisoned) => {}
            other => panic!("expected MutexPoisoned, got {other:?}"),
        }
    }

    #[test]
    fn test_rotate_surfaces_poisoned_segment_start_seq() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| panic!("tempdir"));

        // Size the segment so exactly one entry fits and the second append must
        // rotate (which locks segment_start_seq).
        let entry_total = FileJournal::<()>::encode_entry(&make_event(0))
            .unwrap_or_else(|_| panic!("encode"))
            .len();
        let segment_size = entry_total + 8; // >= one entry, < two entries
        let journal = FileJournal::<()>::open_with_segment_size(dir.path(), segment_size)
            .unwrap_or_else(|_| panic!("open"));

        assert!(journal.append(&make_event(0)).is_ok(), "first entry fits");

        poison(&journal.segment_start_seq);

        // The second append triggers rotation, which must surface the poison.
        match journal.append(&make_event(1)) {
            Err(JournalError::MutexPoisoned) => {}
            other => panic!("expected MutexPoisoned from rotation, got {other:?}"),
        }
    }

    #[test]
    fn test_archive_segments_before() {
        let dir = tempfile::tempdir();
        assert!(dir.is_ok());
        let dir = dir.unwrap_or_else(|_| panic!("tempdir"));

        // Small segment size to force rotation
        let journal = FileJournal::<()>::open_with_segment_size(dir.path(), 512);
        assert!(journal.is_ok());
        let journal = journal.unwrap_or_else(|_| panic!("open"));

        for i in 0..20 {
            let event = make_event(i);
            let result = journal.append(&event);
            assert!(result.is_ok());
        }

        let segments_before = list_segments(dir.path()).unwrap_or_default();
        assert!(segments_before.len() > 1);

        // Archive all segments before the last one
        let last_start = *segments_before.iter().max().unwrap_or(&0);
        let archived = journal.archive_segments_before(last_start);
        assert!(archived.is_ok());
        let archived_count = archived.unwrap_or(0);
        assert!(archived_count > 0);

        // Verify active segments decreased
        let segments_after = list_segments(dir.path()).unwrap_or_default();
        assert!(segments_after.len() < segments_before.len());
    }

    #[test]
    fn test_reopen_journal_resumes() {
        let dir = tempfile::tempdir();
        assert!(dir.is_ok());
        let dir = dir.unwrap_or_else(|_| panic!("tempdir"));

        // Write some entries
        {
            let journal = FileJournal::<()>::open(dir.path());
            assert!(journal.is_ok());
            let journal = journal.unwrap_or_else(|_| panic!("open"));

            for i in 0..5 {
                let event = make_event(i);
                let result = journal.append(&event);
                assert!(result.is_ok());
            }
        }

        // Re-open and continue writing
        {
            let journal = FileJournal::<()>::open(dir.path());
            assert!(journal.is_ok());
            let journal = journal.unwrap_or_else(|_| panic!("reopen"));

            assert_eq!(journal.last_sequence(), Some(4));

            for i in 5..10 {
                let event = make_event(i);
                let result = journal.append(&event);
                assert!(result.is_ok());
            }

            assert_eq!(journal.last_sequence(), Some(9));

            // Read all entries
            let entries: Vec<_> = journal
                .read_from(0)
                .unwrap_or_else(|_| panic!("read_from"))
                .collect();
            assert_eq!(entries.len(), 10);
        }
    }

    #[test]
    fn test_segment_path_format() {
        let dir = PathBuf::from("/tmp/journal");
        let path = segment_path(&dir, 42);
        assert_eq!(
            path.to_string_lossy(),
            "/tmp/journal/segment-00000000000000000042.journal"
        );
    }

    #[test]
    fn test_entry_overhead_constant() {
        assert_eq!(super::super::journal::ENTRY_OVERHEAD, 24);
        assert_eq!(ENTRY_HEADER_SIZE, 20);
        assert_eq!(ENTRY_CRC_SIZE, 4);
    }

    #[test]
    fn test_journal_error_display() {
        let err = JournalError::CorruptEntry {
            sequence: 42,
            expected_crc: 0xDEAD_BEEF,
            actual_crc: 0xCAFE_BABE,
        };
        let display = format!("{err}");
        assert!(display.contains("corrupt journal entry"));
        assert!(display.contains("42"));

        let err2 = JournalError::MutexPoisoned;
        let display2 = format!("{err2}");
        assert!(display2.contains("mutex poisoned"));
    }

    #[test]
    fn test_sequencer_event_serialize_roundtrip() {
        let event = make_event(7);
        let json = serde_json::to_vec(&event);
        assert!(json.is_ok());
        let bytes = json.unwrap_or_default();

        let decoded: Result<SequencerEvent<()>, _> = serde_json::from_slice(&bytes);
        assert!(decoded.is_ok());
        let decoded = decoded.unwrap_or_else(|_| panic!("decode"));
        assert_eq!(decoded.sequence_num, 7);
    }
}
