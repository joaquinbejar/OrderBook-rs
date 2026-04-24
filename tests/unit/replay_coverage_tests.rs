/******************************************************************************
   Unit tests for ReplayEngine and InMemoryJournal coverage gaps.
   Covers: replay error paths, snapshots_match, JournalError Display,
   InMemoryJournal edge cases.
******************************************************************************/

use orderbook_rs::orderbook::mass_cancel::MassCancelResult;
use orderbook_rs::orderbook::sequencer::{
    InMemoryJournal, Journal, ReplayEngine, ReplayError, SequencerCommand, SequencerEvent,
    SequencerResult, snapshots_match,
};
use pricelevel::{Hash32, Id, Price, Quantity, Side, TimeInForce, TimestampMs};

// ─── Helper ─────────────────────────────────────────────────────────────────

fn make_add_event(seq: u64, id: Id, price: u128, qty: u64, side: Side) -> SequencerEvent<()> {
    use pricelevel::OrderType;
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

fn make_cancel_event(seq: u64, id: Id) -> SequencerEvent<()> {
    SequencerEvent {
        sequence_num: seq,
        timestamp_ns: 0,
        command: SequencerCommand::CancelOrder(id),
        result: SequencerResult::OrderCancelled { order_id: id },
    }
}

fn make_rejected_event(seq: u64) -> SequencerEvent<()> {
    SequencerEvent {
        sequence_num: seq,
        timestamp_ns: 0,
        command: SequencerCommand::CancelAll,
        result: SequencerResult::Rejected {
            reason: "test rejection".to_string(),
        },
    }
}

// ─── ReplayEngine ───────────────────────────────────────────────────────────

#[test]
fn replay_empty_journal_returns_error() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let result = ReplayEngine::<()>::replay_from(&journal, 0, "TEST");
    assert!(result.is_err());
    let err = result.err().expect("expected error");
    assert!(matches!(err, ReplayError::EmptyJournal));
    // Verify Display impl
    assert!(err.to_string().contains("empty"));
}

#[test]
fn replay_invalid_sequence_returns_error() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    let event = make_add_event(0, id, 100, 10, Side::Buy);
    assert!(journal.append(&event).is_ok());

    let result = ReplayEngine::<()>::replay_from(&journal, 999, "TEST");
    assert!(result.is_err());
    let err = result.err().expect("expected error");
    assert!(matches!(
        err,
        ReplayError::InvalidSequence {
            from_sequence: 999,
            last_sequence: 0,
        }
    ));
    assert!(err.to_string().contains("999"));
}

#[test]
fn replay_sequence_gap_returns_error() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id1 = Id::new_uuid();
    let id2 = Id::new_uuid();

    // Append events with a gap: 0, then 5 (skipping 1-4)
    assert!(
        journal
            .append(&make_add_event(0, id1, 100, 10, Side::Buy))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_add_event(5, id2, 200, 20, Side::Sell))
            .is_ok()
    );

    let result = ReplayEngine::<()>::replay_from(&journal, 0, "TEST");
    assert!(result.is_err());
    let err = result.err().expect("expected error");
    assert!(matches!(
        err,
        ReplayError::SequenceGap {
            expected: 1,
            found: 5,
        }
    ));
    assert!(err.to_string().contains("gap"));
}

#[test]
fn replay_single_add_order() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id, 100, 10, Side::Buy))
            .is_ok()
    );

    let result = ReplayEngine::<()>::replay_from(&journal, 0, "TEST");
    assert!(result.is_ok());
    let (book, last_seq) = result.unwrap();
    assert_eq!(last_seq, 0);
    let snap = book.create_snapshot(usize::MAX);
    assert_eq!(snap.bids.len(), 1);
}

#[test]
fn replay_add_then_cancel() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id, 100, 10, Side::Buy))
            .is_ok()
    );
    assert!(journal.append(&make_cancel_event(1, id)).is_ok());

    let result = ReplayEngine::<()>::replay_from(&journal, 0, "TEST");
    assert!(result.is_ok());
    let (book, last_seq) = result.unwrap();
    assert_eq!(last_seq, 1);
    let snap = book.create_snapshot(usize::MAX);
    assert!(snap.bids.is_empty());
    assert!(snap.asks.is_empty());
}

#[test]
fn replay_skips_rejected_events() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id, 100, 10, Side::Buy))
            .is_ok()
    );
    assert!(journal.append(&make_rejected_event(1)).is_ok());

    let result = ReplayEngine::<()>::replay_from(&journal, 0, "TEST");
    assert!(result.is_ok());
    let (book, last_seq) = result.unwrap();
    // The add at seq 0 is applied, the rejected event at seq 1 is skipped
    // and therefore does not advance `last_applied_seq`.
    assert_eq!(last_seq, 0);
    let snap = book.create_snapshot(usize::MAX);
    assert_eq!(snap.bids.len(), 1);
}

#[test]
fn replay_with_progress_callback() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id1 = Id::new_uuid();
    let id2 = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id1, 100, 10, Side::Buy))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_add_event(1, id2, 200, 20, Side::Sell))
            .is_ok()
    );

    let progress_calls = std::cell::RefCell::new(Vec::new());
    let result =
        ReplayEngine::<()>::replay_from_with_progress(&journal, 0, "TEST", |count, seq| {
            progress_calls.borrow_mut().push((count, seq));
        });
    assert!(result.is_ok());
    let calls = progress_calls.into_inner();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], (1, 0));
    assert_eq!(calls[1], (2, 1));
}

#[test]
fn replay_cancel_all_command() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id, 100, 10, Side::Buy))
            .is_ok()
    );

    let cancel_all_event = SequencerEvent {
        sequence_num: 1,
        timestamp_ns: 0,
        command: SequencerCommand::CancelAll,
        result: SequencerResult::MassCancelled {
            result: MassCancelResult::default(),
        },
    };
    assert!(journal.append(&cancel_all_event).is_ok());

    let result = ReplayEngine::<()>::replay_from(&journal, 0, "TEST");
    assert!(result.is_ok());
    let (book, _) = result.unwrap();
    let snap = book.create_snapshot(usize::MAX);
    assert!(snap.bids.is_empty());
}

#[test]
fn replay_cancel_by_side_command() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id, 100, 10, Side::Buy))
            .is_ok()
    );

    let cancel_side_event = SequencerEvent {
        sequence_num: 1,
        timestamp_ns: 0,
        command: SequencerCommand::CancelBySide { side: Side::Buy },
        result: SequencerResult::MassCancelled {
            result: MassCancelResult::default(),
        },
    };
    assert!(journal.append(&cancel_side_event).is_ok());

    let result = ReplayEngine::<()>::replay_from(&journal, 0, "TEST");
    assert!(result.is_ok());
    let (book, _) = result.unwrap();
    let snap = book.create_snapshot(usize::MAX);
    assert!(snap.bids.is_empty());
}

#[test]
fn replay_cancel_by_user_command() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();

    let add_event = make_add_event(0, id, 100, 10, Side::Buy);
    assert!(journal.append(&add_event).is_ok());

    let cancel_user_event = SequencerEvent {
        sequence_num: 1,
        timestamp_ns: 0,
        command: SequencerCommand::CancelByUser {
            user_id: Hash32::from([42u8; 32]),
        },
        result: SequencerResult::MassCancelled {
            result: MassCancelResult::default(),
        },
    };
    assert!(journal.append(&cancel_user_event).is_ok());

    let result = ReplayEngine::<()>::replay_from(&journal, 0, "TEST");
    assert!(result.is_ok());
}

#[test]
fn replay_cancel_by_price_range_command() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id, 100, 10, Side::Buy))
            .is_ok()
    );

    let cancel_range_event = SequencerEvent {
        sequence_num: 1,
        timestamp_ns: 0,
        command: SequencerCommand::CancelByPriceRange {
            side: Side::Buy,
            min_price: 50,
            max_price: 150,
        },
        result: SequencerResult::MassCancelled {
            result: MassCancelResult::default(),
        },
    };
    assert!(journal.append(&cancel_range_event).is_ok());

    let result = ReplayEngine::<()>::replay_from(&journal, 0, "TEST");
    assert!(result.is_ok());
}

#[test]
fn replay_market_order_command() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id1 = Id::new_uuid();
    let id2 = Id::new_uuid();
    // Add a sell order for the market buy to match against
    assert!(
        journal
            .append(&make_add_event(0, id1, 100, 10, Side::Sell))
            .is_ok()
    );

    let market_event = SequencerEvent {
        sequence_num: 1,
        timestamp_ns: 0,
        command: SequencerCommand::MarketOrder {
            id: id2,
            quantity: 5,
            side: Side::Buy,
        },
        result: SequencerResult::TradeExecuted {
            trade_result: orderbook_rs::TradeResult::new(
                "TEST".to_string(),
                pricelevel::MatchResult::new(id2, 5),
            ),
        },
    };
    assert!(journal.append(&market_event).is_ok());

    let result = ReplayEngine::<()>::replay_from(&journal, 0, "TEST");
    assert!(result.is_ok());
}

#[test]
fn replay_verify_matching_snapshot() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id, 100, 10, Side::Buy))
            .is_ok()
    );

    // Replay to get the expected state
    let (book, _) = ReplayEngine::<()>::replay_from(&journal, 0, "TEST").unwrap();
    let snapshot = book.create_snapshot(usize::MAX);

    let result = ReplayEngine::<()>::verify(&journal, &snapshot);
    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[test]
fn replay_verify_mismatched_snapshot() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id, 100, 10, Side::Buy))
            .is_ok()
    );

    // Create a snapshot from a different book
    let other_book = orderbook_rs::OrderBook::<()>::new("TEST");
    let _ = other_book.add_limit_order(Id::new_uuid(), 999, 99, Side::Sell, TimeInForce::Gtc, None);
    let wrong_snapshot = other_book.create_snapshot(usize::MAX);

    let result = ReplayEngine::<()>::verify(&journal, &wrong_snapshot);
    assert!(result.is_ok());
    assert!(!result.unwrap()); // Should not match
}

// ─── snapshots_match ────────────────────────────────────────────────────────

#[test]
fn snapshots_match_identical_books() {
    let book = orderbook_rs::OrderBook::<()>::new("TEST");
    let _ = book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new_uuid(), 110, 5, Side::Sell, TimeInForce::Gtc, None);
    let s1 = book.create_snapshot(usize::MAX);
    let s2 = book.create_snapshot(usize::MAX);
    assert!(snapshots_match(&s1, &s2));
}

#[test]
fn snapshots_match_different_symbols_returns_false() {
    let book1 = orderbook_rs::OrderBook::<()>::new("BTC");
    let book2 = orderbook_rs::OrderBook::<()>::new("ETH");
    let s1 = book1.create_snapshot(usize::MAX);
    let s2 = book2.create_snapshot(usize::MAX);
    assert!(!snapshots_match(&s1, &s2));
}

#[test]
fn snapshots_match_different_bids_returns_false() {
    let book1 = orderbook_rs::OrderBook::<()>::new("TEST");
    let _ = book1.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let book2 = orderbook_rs::OrderBook::<()>::new("TEST");
    let _ = book2.add_limit_order(Id::new_uuid(), 200, 20, Side::Buy, TimeInForce::Gtc, None);
    let s1 = book1.create_snapshot(usize::MAX);
    let s2 = book2.create_snapshot(usize::MAX);
    assert!(!snapshots_match(&s1, &s2));
}

#[test]
fn snapshots_match_different_asks_returns_false() {
    let book1 = orderbook_rs::OrderBook::<()>::new("TEST");
    let _ = book1.add_limit_order(Id::new_uuid(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
    let book2 = orderbook_rs::OrderBook::<()>::new("TEST");
    let _ = book2.add_limit_order(Id::new_uuid(), 200, 20, Side::Sell, TimeInForce::Gtc, None);
    let s1 = book1.create_snapshot(usize::MAX);
    let s2 = book2.create_snapshot(usize::MAX);
    assert!(!snapshots_match(&s1, &s2));
}

#[test]
fn snapshots_match_different_bid_count_returns_false() {
    let book1 = orderbook_rs::OrderBook::<()>::new("TEST");
    let _ = book1.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let book2 = orderbook_rs::OrderBook::<()>::new("TEST");
    let s1 = book1.create_snapshot(usize::MAX);
    let s2 = book2.create_snapshot(usize::MAX);
    assert!(!snapshots_match(&s1, &s2));
}

#[test]
fn snapshots_match_different_ask_count_returns_false() {
    let book1 = orderbook_rs::OrderBook::<()>::new("TEST");
    let _ = book1.add_limit_order(Id::new_uuid(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
    let book2 = orderbook_rs::OrderBook::<()>::new("TEST");
    let s1 = book1.create_snapshot(usize::MAX);
    let s2 = book2.create_snapshot(usize::MAX);
    assert!(!snapshots_match(&s1, &s2));
}

// ─── InMemoryJournal ────────────────────────────────────────────────────────

#[test]
fn in_memory_journal_new_is_empty() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    assert!(journal.is_empty());
    assert_eq!(journal.len(), 0);
    assert_eq!(journal.last_sequence(), None);
}

#[test]
fn in_memory_journal_default_is_empty() {
    let journal: InMemoryJournal<()> = InMemoryJournal::default();
    assert!(journal.is_empty());
}

#[test]
fn in_memory_journal_with_capacity() {
    let journal: InMemoryJournal<()> = InMemoryJournal::with_capacity(100);
    assert!(journal.is_empty());
    assert_eq!(journal.len(), 0);
}

#[test]
fn in_memory_journal_append_and_len() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    let event = make_add_event(0, id, 100, 10, Side::Buy);
    assert!(journal.append(&event).is_ok());
    assert!(!journal.is_empty());
    assert_eq!(journal.len(), 1);
    assert_eq!(journal.last_sequence(), Some(0));
}

#[test]
fn in_memory_journal_read_from_filters_correctly() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id1 = Id::new_uuid();
    let id2 = Id::new_uuid();
    let id3 = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id1, 100, 10, Side::Buy))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_add_event(1, id2, 200, 20, Side::Sell))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_add_event(2, id3, 300, 30, Side::Buy))
            .is_ok()
    );

    // Read from sequence 1 should give 2 entries
    let iter = journal.read_from(1);
    assert!(iter.is_ok());
    let entries: Vec<_> = iter.unwrap().collect();
    assert_eq!(entries.len(), 2);
}

#[test]
fn in_memory_journal_verify_integrity_always_ok() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    assert!(journal.verify_integrity().is_ok());
}

// ─── JournalError Display ───────────────────────────────────────────────────

#[test]
fn journal_error_display_io_with_path() {
    let path = std::env::temp_dir().join("journal");
    let path_str = path.to_string_lossy().into_owned();
    let err = orderbook_rs::JournalError::Io {
        message: "disk full".to_string(),
        path: Some(path),
    };
    let display = err.to_string();
    assert!(display.contains("disk full"));
    assert!(display.contains(&path_str));
}

#[test]
fn journal_error_display_io_without_path() {
    let err = orderbook_rs::JournalError::Io {
        message: "connection reset".to_string(),
        path: None,
    };
    let display = err.to_string();
    assert!(display.contains("connection reset"));
}

#[test]
fn journal_error_display_corrupt_entry() {
    let err = orderbook_rs::JournalError::CorruptEntry {
        sequence: 42,
        expected_crc: 0xDEAD_BEEF,
        actual_crc: 0xCAFE_BABE,
    };
    let display = err.to_string();
    assert!(display.contains("42"));
    assert!(display.contains("corrupt"));
}

#[test]
fn journal_error_display_deserialization() {
    let err = orderbook_rs::JournalError::DeserializationError {
        sequence: 7,
        message: "invalid utf-8".to_string(),
    };
    let display = err.to_string();
    assert!(display.contains("7"));
    assert!(display.contains("invalid utf-8"));
}

#[test]
fn journal_error_display_serialization() {
    let err = orderbook_rs::JournalError::SerializationError {
        message: "buffer overflow".to_string(),
    };
    let display = err.to_string();
    assert!(display.contains("buffer overflow"));
}

#[test]
fn journal_error_display_entry_too_large() {
    let err = orderbook_rs::JournalError::EntryTooLarge {
        entry_bytes: 1_000_000,
        segment_size: 100_000,
    };
    let display = err.to_string();
    assert!(display.contains("1000000"));
    assert!(display.contains("100000"));
}

#[test]
fn journal_error_display_invalid_directory() {
    let err = orderbook_rs::JournalError::InvalidDirectory {
        path: std::path::PathBuf::from("/nonexistent"),
    };
    let display = err.to_string();
    assert!(display.contains("/nonexistent"));
}

#[test]
fn journal_error_display_mutex_poisoned() {
    let err = orderbook_rs::JournalError::MutexPoisoned;
    let display = err.to_string();
    assert!(display.contains("mutex") || display.contains("poisoned"));
}

#[test]
fn journal_error_display_sequence_not_found() {
    let err = orderbook_rs::JournalError::SequenceNotFound { sequence: 999 };
    let display = err.to_string();
    assert!(display.contains("999"));
}

#[test]
fn journal_error_display_invalid_entry_header() {
    let err = orderbook_rs::JournalError::InvalidEntryHeader {
        offset: 128,
        message: "truncated".to_string(),
    };
    let display = err.to_string();
    assert!(display.contains("128"));
    assert!(display.contains("truncated"));
}

#[test]
fn journal_error_from_io_error() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    let journal_err: orderbook_rs::JournalError = io_err.into();
    let display = journal_err.to_string();
    assert!(display.contains("file not found"));
}

// ─── ReplayError Display ────────────────────────────────────────────────────

#[test]
fn replay_error_snapshot_mismatch_display() {
    let err = ReplayError::SnapshotMismatch;
    assert!(err.to_string().contains("mismatch"));
}
