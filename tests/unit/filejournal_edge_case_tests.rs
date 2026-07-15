//! Edge case tests for `FileJournal` covering crash recovery, segment
//! rotation stress, concurrent reader/writer, large entry boundaries,
//! empty journal operations, and archived segment handling.

#[cfg(feature = "journal")]
#[cfg(test)]
mod tests_filejournal_edge_cases {
    use orderbook_rs::orderbook::sequencer::journal::Journal;
    use orderbook_rs::orderbook::sequencer::{
        FileJournal, SequencerCommand, SequencerEvent, SequencerResult,
    };
    use pricelevel::Id;
    use std::fs;
    use std::path::Path;

    fn make_event(seq: u64) -> SequencerEvent<()> {
        SequencerEvent {
            sequence_num: seq,
            timestamp_ns: 1_700_000_000_000_000_000u64.saturating_add(seq),
            command: SequencerCommand::CancelOrder(Id::new_uuid()),
            result: SequencerResult::OrderCancelled {
                order_id: Id::new_uuid(),
            },
        }
    }

    /// Build the segment file path for a given start sequence.
    fn segment_path(dir: &Path, start_seq: u64) -> std::path::PathBuf {
        dir.join(format!("segment-{start_seq:020}.journal"))
    }

    /// List active (non-archived) segment start sequences in a directory.
    fn list_segments(dir: &Path) -> Vec<u64> {
        let mut seqs = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if let Some(rest) = name_str.strip_prefix("segment-")
                    && let Some(seq_str) = rest.strip_suffix(".journal")
                    && let Ok(seq) = seq_str.parse::<u64>()
                {
                    seqs.push(seq);
                }
            }
        }
        seqs.sort();
        seqs
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 1. Crash Recovery / Partial Write
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn crash_recovery_truncated_last_entry_detected_by_verify() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> = FileJournal::open(dir.path()).expect("open journal");

        for i in 0..5 {
            journal.append(&make_event(i)).expect("append");
        }
        assert!(journal.verify_integrity().is_ok());

        // Drop to release the mmap
        drop(journal);

        // Corrupt: truncate the file to cut into the last entry
        let segs = list_segments(dir.path());
        assert!(!segs.is_empty());
        let seg_path = segment_path(dir.path(), segs[0]);
        let data = fs::read(&seg_path).expect("read segment");

        // Find the actual used length by scanning for zero entry_length
        let mut used_len = 0;
        let mut offset = 0;
        while offset + 4 <= data.len() {
            let el = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            if el == 0 {
                break;
            }
            let end = offset + 4 + el;
            if end > data.len() {
                break;
            }
            used_len = end;
            offset = end;
        }

        // Truncate to remove half of the last entry (simulate crash mid-write)
        let truncated_len = used_len.saturating_sub(10).max(1);
        fs::write(&seg_path, &data[..truncated_len]).expect("write truncated");

        // Re-open and verify — should detect the truncation
        let journal2: FileJournal<()> = FileJournal::open(dir.path()).expect("reopen");
        let integrity = journal2.verify_integrity();
        // Should detect corruption or truncation
        assert!(
            integrity.is_err(),
            "verify_integrity should detect truncated entry"
        );
    }

    #[test]
    fn crash_recovery_appends_continue_after_truncated_entry() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> = FileJournal::open(dir.path()).expect("open journal");

        for i in 0..3 {
            journal.append(&make_event(i)).expect("append");
        }
        drop(journal);

        // Truncate the segment file at a valid entry boundary minus a few bytes
        let segs = list_segments(dir.path());
        let seg_path = segment_path(dir.path(), segs[0]);
        let data = fs::read(&seg_path).expect("read segment");

        // Find offset of 2nd entry to truncate after it (losing 3rd entry)
        let mut offsets = Vec::new();
        let mut off = 0;
        while off + 4 <= data.len() {
            let el = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
                as usize;
            if el == 0 {
                break;
            }
            let end = off + 4 + el;
            if end > data.len() {
                break;
            }
            offsets.push(end);
            off = end;
        }

        // Keep only first 2 entries
        assert!(offsets.len() >= 2);
        let truncated = offsets[1];
        // Zero out everything after the 2nd entry
        let mut new_data = data[..truncated].to_vec();
        new_data.resize(data.len(), 0); // Pad with zeros to keep file size
        fs::write(&seg_path, &new_data).expect("write truncated");

        // Re-open — should see only 2 entries and allow appending
        let journal2: FileJournal<()> = FileJournal::open(dir.path()).expect("reopen");
        assert_eq!(journal2.last_sequence(), Some(1));

        // Append new entries starting from seq 2
        journal2
            .append(&make_event(2))
            .expect("append after recovery");
        journal2
            .append(&make_event(3))
            .expect("append after recovery");
        assert_eq!(journal2.last_sequence(), Some(3));

        // Read all entries — should have 4
        let entries: Vec<_> = journal2
            .read_from(0)
            .expect("read_from")
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 4);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 2. Segment Rotation Under Load
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn stress_rapid_rotations_many_entries() {
        let dir = tempfile::tempdir().expect("create temp dir");
        // Very small segment size: forces rotation every ~2 entries
        let journal: FileJournal<()> =
            FileJournal::open_with_segment_size(dir.path(), 400).expect("open journal");

        let count = 200u64;
        for i in 0..count {
            journal.append(&make_event(i)).expect("append");
        }

        assert_eq!(journal.last_sequence(), Some(count - 1));

        // Should have many segment files
        let segs = list_segments(dir.path());
        assert!(
            segs.len() > 10,
            "expected many segments from rapid rotation, got {}",
            segs.len()
        );

        // Read all entries back seamlessly
        let entries: Vec<_> = journal
            .read_from(0)
            .expect("read_from")
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), count as usize);

        // Verify ordering
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.event.sequence_num, i as u64);
        }
    }

    #[test]
    fn stress_read_from_middle_across_segments() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> =
            FileJournal::open_with_segment_size(dir.path(), 400).expect("open journal");

        for i in 0..50 {
            journal.append(&make_event(i)).expect("append");
        }

        // Read from seq 25 — should cross segment boundaries
        let entries: Vec<_> = journal
            .read_from(25)
            .expect("read_from")
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 25);
        assert_eq!(entries[0].event.sequence_num, 25);
        assert_eq!(entries[24].event.sequence_num, 49);
    }

    #[test]
    fn stress_last_sequence_correct_after_many_rotations() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> =
            FileJournal::open_with_segment_size(dir.path(), 400).expect("open journal");

        for i in 0..100 {
            journal.append(&make_event(i)).expect("append");
            assert_eq!(
                journal.last_sequence(),
                Some(i),
                "last_sequence wrong after append #{i}"
            );
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 3. Concurrent Reader + Writer
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn concurrent_reader_writer_no_panic() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: Arc<FileJournal<()>> =
            Arc::new(FileJournal::open_with_segment_size(dir.path(), 1024).expect("open journal"));

        let writer_journal = Arc::clone(&journal);
        let writer = thread::spawn(move || {
            for i in 0..100 {
                writer_journal.append(&make_event(i)).expect("append");
            }
        });

        let reader_journal = Arc::clone(&journal);
        let reader = thread::spawn(move || {
            let mut entries_seen = 0usize;
            // Poll to exercise concurrent reading until at least one entry
            // is observed (bounded, so a starved writer on a loaded CI
            // runner cannot flake the test; the fixed 20×1ms loop used to
            // time out under coverage instrumentation, and `max_seen > 0`
            // also mis-fired when only sequence 0 had been written).
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if let Ok(iter) = reader_journal.read_from(0) {
                    entries_seen = entries_seen.max(iter.flatten().count());
                }
                if entries_seen > 0 {
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(1));
            }
            entries_seen
        });

        writer.join().expect("writer thread panicked");
        let entries_seen = reader.join().expect("reader thread panicked");

        // Reader should have seen at least some entries
        assert!(entries_seen > 0, "reader should have seen entries");

        // After writer completes, all 100 entries must be readable
        let all_entries: Vec<_> = journal
            .read_from(0)
            .expect("read_from")
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(all_entries.len(), 100);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 4. Large Entries Near Segment Boundary
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn large_entry_triggers_rotation_when_near_boundary() {
        let dir = tempfile::tempdir().expect("create temp dir");
        // Segment size just big enough for ~2 entries
        let journal: FileJournal<()> =
            FileJournal::open_with_segment_size(dir.path(), 500).expect("open journal");

        // Write 3 entries — the 3rd should trigger rotation
        for i in 0..3 {
            journal.append(&make_event(i)).expect("append");
        }

        let segs = list_segments(dir.path());
        assert!(
            segs.len() >= 2,
            "expected at least 2 segments, got {}",
            segs.len()
        );

        // All entries readable
        let entries: Vec<_> = journal
            .read_from(0)
            .expect("read_from")
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn entry_too_large_for_any_segment_returns_error() {
        let dir = tempfile::tempdir().expect("create temp dir");
        // Tiny segment that can't fit even one entry
        let journal: FileJournal<()> =
            FileJournal::open_with_segment_size(dir.path(), 50).expect("open journal");

        let result = journal.append(&make_event(0));
        assert!(result.is_err());
        if let Err(err) = result {
            let msg = format!("{err}");
            assert!(
                msg.contains("too large") || msg.contains("entry"),
                "expected EntryTooLarge error, got: {msg}"
            );
        }
    }

    #[test]
    fn entry_exactly_fills_segment_then_rotates() {
        let dir = tempfile::tempdir().expect("create temp dir");

        // Measure the size of one encoded event to calibrate segment size
        let sample_event = make_event(0);
        let encoded = serde_json::to_vec(&sample_event).expect("encode");
        // Total entry size: 4 (entry_length) + 8 (seq) + 8 (ts) + payload + 4 (crc)
        let entry_size = 4 + 8 + 8 + encoded.len() + 4;

        // Segment size = exactly 2 entries
        let segment_size = entry_size * 2;
        let journal: FileJournal<()> =
            FileJournal::open_with_segment_size(dir.path(), segment_size).expect("open journal");

        // Write 2 entries — should fill first segment exactly
        journal.append(&make_event(0)).expect("append 0");
        journal.append(&make_event(1)).expect("append 1");

        // 3rd entry should trigger rotation
        journal.append(&make_event(2)).expect("append 2");

        let segs = list_segments(dir.path());
        assert_eq!(segs.len(), 2, "expected exactly 2 segments");

        let entries: Vec<_> = journal
            .read_from(0)
            .expect("read_from")
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 3);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 5. Empty Journal Operations
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn empty_journal_read_from_zero_returns_empty_iterator() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> = FileJournal::open(dir.path()).expect("open journal");

        let entries: Vec<_> = journal.read_from(0).expect("read_from").collect::<Vec<_>>();
        assert!(entries.is_empty());
    }

    #[test]
    fn empty_journal_last_sequence_returns_none() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> = FileJournal::open(dir.path()).expect("open journal");

        assert_eq!(journal.last_sequence(), None);
    }

    #[test]
    fn empty_journal_verify_integrity_succeeds() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> = FileJournal::open(dir.path()).expect("open journal");

        assert!(journal.verify_integrity().is_ok());
    }

    #[test]
    fn empty_journal_read_from_high_sequence_returns_empty() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> = FileJournal::open(dir.path()).expect("open journal");

        let entries: Vec<_> = journal
            .read_from(999)
            .expect("read_from")
            .collect::<Vec<_>>();
        assert!(entries.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 6. Archived Segment Handling
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn archived_segments_excluded_from_read() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> =
            FileJournal::open_with_segment_size(dir.path(), 400).expect("open journal");

        for i in 0..30 {
            journal.append(&make_event(i)).expect("append");
        }

        let segs_before = list_segments(dir.path());
        assert!(segs_before.len() > 1);

        // Archive all segments except the active one
        let active_start = *segs_before.last().expect("at least one segment");
        let archived = journal
            .archive_segments_before(active_start)
            .expect("archive");
        assert!(archived > 0);

        // Active segments should be fewer
        let segs_after = list_segments(dir.path());
        assert!(segs_after.len() < segs_before.len());

        // Reading from seq 0 should only return entries from non-archived segments
        let entries: Vec<_> = journal
            .read_from(0)
            .expect("read_from")
            .filter_map(|e| e.ok())
            .collect();

        // Entries from archived segments should be gone
        assert!(
            entries.len() < 30,
            "expected fewer than 30 entries after archival, got {}",
            entries.len()
        );

        // All returned entries should have sequence >= active_start
        for entry in &entries {
            assert!(
                entry.event.sequence_num >= active_start,
                "entry seq {} should be >= active start {}",
                entry.event.sequence_num,
                active_start
            );
        }
    }

    #[test]
    fn verify_integrity_works_after_archival() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> =
            FileJournal::open_with_segment_size(dir.path(), 400).expect("open journal");

        for i in 0..30 {
            journal.append(&make_event(i)).expect("append");
        }

        let segs = list_segments(dir.path());
        let active_start = *segs.last().expect("at least one segment");
        journal
            .archive_segments_before(active_start)
            .expect("archive");

        // Integrity check should pass on remaining active segments
        assert!(journal.verify_integrity().is_ok());
    }

    #[test]
    fn archived_files_exist_on_disk() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let journal: FileJournal<()> =
            FileJournal::open_with_segment_size(dir.path(), 400).expect("open journal");

        for i in 0..30 {
            journal.append(&make_event(i)).expect("append");
        }

        let segs = list_segments(dir.path());
        let active_start = *segs.last().expect("at least one segment");
        let archived_count = journal
            .archive_segments_before(active_start)
            .expect("archive");

        // Count .journal.archived files
        let mut archived_files = 0usize;
        if let Ok(entries) = fs::read_dir(dir.path()) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().ends_with(".journal.archived") {
                    archived_files += 1;
                }
            }
        }

        assert_eq!(
            archived_files, archived_count,
            "archived file count should match archive_segments_before return value"
        );
    }

    #[test]
    fn reopen_after_archival_resumes_correctly() {
        let dir = tempfile::tempdir().expect("create temp dir");

        // Write, rotate, and archive
        {
            let journal: FileJournal<()> =
                FileJournal::open_with_segment_size(dir.path(), 400).expect("open journal");

            for i in 0..30 {
                journal.append(&make_event(i)).expect("append");
            }

            let segs = list_segments(dir.path());
            let active_start = *segs.last().expect("at least one segment");
            journal
                .archive_segments_before(active_start)
                .expect("archive");
        }

        // Reopen and continue writing
        let journal2: FileJournal<()> =
            FileJournal::open_with_segment_size(dir.path(), 400).expect("reopen");

        let last = journal2.last_sequence();
        assert!(last.is_some());

        let next_seq = last.expect("has last") + 1;
        journal2
            .append(&make_event(next_seq))
            .expect("append after reopen");
        assert_eq!(journal2.last_sequence(), Some(next_seq));
    }
}
